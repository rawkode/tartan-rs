//! Extension packages in object storage, their compiled components, and registering a package
//! with the forge.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use rivetkit::client::Client;
use tartan_extensions::{ExtensionComponent, ExtensionPackage, ExtensionRuntime, Manifest};
use tokio::sync::Mutex;

use crate::{PackageInfo, PackageQuery, PublishPackage, forge_handle};

/// Stores packages and runs their components, and knows which packages the deployment makes
/// available. One per process.
pub struct Extensions {
    runtime: ExtensionRuntime,
    store: Arc<dyn ObjectStore>,
    loaded: Mutex<HashMap<String, Loaded>>,
    /// The `id@version` of every package in a module the deployment lists, as this process's
    /// module sync found them; `None` while the sync runs.
    available: tokio::sync::watch::Sender<Option<Arc<BTreeSet<String>>>>,
}

/// A package and its compiled component.
#[derive(Clone)]
pub struct Loaded {
    /// The package.
    pub package: Arc<ExtensionPackage>,
    /// Its compiled component.
    pub component: ExtensionComponent,
}

impl Extensions {
    /// Packages stored in `store`, run by `runtime`.
    pub fn new(runtime: ExtensionRuntime, store: Arc<dyn ObjectStore>) -> Self {
        Self {
            runtime,
            store,
            loaded: Mutex::new(HashMap::new()),
            available: tokio::sync::watch::Sender::new(None),
        }
    }

    /// Marks a module sync as started: until [`Extensions::set_available`], what is available
    /// is unknown and [`Extensions::available`] waits.
    pub fn begin_sync(&self) {
        self.available.send_replace(None);
    }

    /// Records the packages the deployment makes available, as `id@version`: those in the
    /// modules it lists. A package registered from a module that is no longer listed stays
    /// registered, so installations of it keep running, but no configuration may name it.
    pub fn set_available(&self, packages: impl IntoIterator<Item = String>) {
        self.available
            .send_replace(Some(Arc::new(packages.into_iter().collect())));
    }

    /// The packages the deployment makes available, as `id@version`, once the module sync has
    /// finished: until then the forge's registry may lack modules the deployment lists, so a
    /// configuration evaluated meanwhile could be refused for a module about to arrive.
    pub async fn available(&self) -> Arc<BTreeSet<String>> {
        let mut receiver = self.available.subscribe();
        match receiver.wait_for(Option::is_some).await {
            Ok(available) => available.clone().unwrap_or_default(),
            // The sender lives as long as `self`, so this is never reached.
            Err(_) => Arc::default(),
        }
    }

    /// Stores `package` under its id, version and digest, and returns the digest.
    /// [`register_package`] checks a package before storing it.
    async fn store(&self, package: &ExtensionPackage) -> Result<String> {
        let digest = package.digest();
        let dir = package_dir(&package.manifest.id, &package.manifest.version, &digest);
        let migrations = serde_json::to_vec(&package.migrations)?;
        // The manifest goes last: a package directory without one is incomplete.
        self.put(&dir, "component.wasm", package.component.clone())
            .await?;
        self.put(&dir, "migrations.json", migrations).await?;
        self.put(
            &dir,
            "tartan.json",
            package.manifest.raw.to_string().into_bytes(),
        )
        .await?;
        Ok(digest)
    }

    /// The package `id@version` with `digest`, compiled. Compiled components are kept for the
    /// life of the process.
    pub async fn load(&self, id: &str, version: &str, digest: &str) -> Result<Loaded> {
        if let Some(loaded) = self.loaded.lock().await.get(digest) {
            return Ok(loaded.clone());
        }
        let dir = package_dir(id, version, digest);
        let manifest = Manifest::parse(&self.get(&dir, "tartan.json").await?)?;
        let component = self.get(&dir, "component.wasm").await?;
        let migrations: Vec<(String, String)> =
            serde_json::from_slice(&self.get(&dir, "migrations.json").await?)?;
        let package = ExtensionPackage {
            manifest,
            component,
            migrations,
        };
        if package.digest() != digest {
            bail!("package {id}@{version} in storage does not match digest {digest}");
        }
        let component = self.runtime.compile(&package.component)?;
        let loaded = Loaded {
            package: Arc::new(package),
            component,
        };
        self.loaded
            .lock()
            .await
            .insert(digest.to_owned(), loaded.clone());
        Ok(loaded)
    }

    async fn put(&self, dir: &str, name: &str, bytes: Vec<u8>) -> Result<()> {
        let path = Path::from(format!("{dir}/{name}"));
        self.store
            .put(&path, PutPayload::from(bytes))
            .await
            .with_context(|| format!("store {path}"))?;
        Ok(())
    }

    async fn get(&self, dir: &str, name: &str) -> Result<Vec<u8>> {
        let path = Path::from(format!("{dir}/{name}"));
        let result = self
            .store
            .get(&path)
            .await
            .with_context(|| format!("read {path}"))?;
        Ok(result.bytes().await?.to_vec())
    }
}

fn package_dir(id: &str, version: &str, digest: &str) -> String {
    format!("packages/{id}/{version}/{digest}")
}

/// Why a package was not registered.
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    /// The package does not meet the forge's checks.
    #[error("{0}")]
    Refused(String),
    /// Its `id@version` is registered with other contents.
    #[error("{0}")]
    Conflict(String),
    /// Storage or the forge actor failed; registering again may succeed.
    #[error("{0:#}")]
    Unavailable(anyhow::Error),
}

/// Registers `package` with the forge, making `id@version` installable, after checking it: its
/// manifest targets this host's extension API and installs on repositories (both checked when it
/// was parsed), its component fits the extension world, and `id@version` is not registered with
/// other contents. Registering the same package again returns it unchanged.
pub async fn register_package(
    client: &Client,
    extensions: &Extensions,
    package: &ExtensionPackage,
) -> Result<PackageInfo, RegisterError> {
    let digest = package.digest();
    let forge = forge_handle(client).map_err(RegisterError::Unavailable)?;
    let existing = forge
        .send(PackageQuery {
            id: package.manifest.id.clone(),
            version: package.manifest.version.clone(),
        })
        .await
        .map_err(RegisterError::Unavailable)?;
    match existing {
        Some(found) if found.digest == digest => return Ok(found),
        Some(found) => {
            return Err(RegisterError::Conflict(format!(
                "{}@{} is already registered with other contents",
                found.id, found.version
            )));
        }
        None => {}
    }
    if let Err(error) = extensions.runtime.compile(&package.component) {
        return Err(RegisterError::Refused(format!(
            "component.wasm is not a component of the extension world: {error:#}"
        )));
    }
    extensions
        .store(package)
        .await
        .map_err(RegisterError::Unavailable)?;
    forge
        .send(PublishPackage {
            package: PackageInfo {
                id: package.manifest.id.clone(),
                version: package.manifest.version.clone(),
                digest,
                manifest: package.manifest.raw.clone(),
            },
        })
        .await
        .map_err(RegisterError::Unavailable)
}

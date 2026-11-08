//! Extension packages in object storage, and their compiled components.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use tartan_extensions::{ExtensionComponent, ExtensionPackage, ExtensionRuntime, Manifest};
use tokio::sync::Mutex;

/// Stores packages and runs their components. One per process; cloning is cheap.
pub struct Extensions {
    runtime: ExtensionRuntime,
    store: Arc<dyn ObjectStore>,
    loaded: Mutex<HashMap<String, Loaded>>,
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
        }
    }

    /// Checks that `package`'s component fits the extension world, then stores the package
    /// under its id, version and digest. Returns the digest.
    pub async fn store(&self, package: &ExtensionPackage) -> Result<String> {
        self.runtime.compile(&package.component)?;
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

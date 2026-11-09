//! Copies the modules the deployment configuration lists into the forge's registry, and
//! registers the extension packages among them.
//!
//! For each listed module the forge pulls the manifest from the module's source registry with
//! ORAS's `oci-client` and copies it with its blobs, unless the registry already holds that
//! version with the same digest. A version the registry holds is never replaced: when a source
//! serves other contents for it, the held copy is kept and the mismatch reported, so a module
//! version, like an `id@version` package, never changes once the forge has it. A source that
//! cannot be reached is reported and the forge starts with the modules it already holds.
//!
//! A module whose zip holds `tartan.json` is an extension: its manifest, component and
//! migrations are read out of the module, checked, and registered (`package.published`). Its
//! manifest's version must be the module's, without the leading `v`.
//!
//! The listed modules are the only ones the registry serves, from the start of the sync, and
//! the packages registered from them are the only ones configurations may install, from its
//! end: a module dropped from the list stays in the store, but neither resolves nor installs.
//!
//! No blob of more than [`MODULE_MAX_BYTES`] is copied, and no module whose files hold more
//! than that, uncompressed, is registered.

use std::collections::BTreeMap;
use std::io::Read;

use anyhow::{Context, Result, anyhow, bail};
use axum::body::Bytes;
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use oci_spec::image::ImageManifest;
use rivetkit::client::Client;
use serde::Serialize;
use tartan_config::deployment::{Deployment, ModuleVersion, Source};
use tartan_extensions::ExtensionPackage;
use tartan_forge::{Extensions, RegisterError, register_package};

use crate::registry::{RegistryStore, digest_of};

/// The media type of a CUE module's zip layer.
const MODULE_ZIP: &str = "application/zip";
/// The most a module's files may hold, uncompressed.
const MODULE_MAX_BYTES: u64 = 64 << 20;

/// What a sync did, module by module.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    /// Modules copied into the registry.
    pub copied: Vec<String>,
    /// Modules the registry already held with the source's digest.
    pub unchanged: Vec<String>,
    /// Modules whose source serves other contents than the registry holds; the held copy is
    /// kept.
    pub mismatched: Vec<Problem>,
    /// Modules that could not be fetched from their source.
    pub unreachable: Vec<Problem>,
    /// Extension packages registered (or already registered), as `id@version`.
    pub registered: Vec<String>,
    /// Modules holding a package that is not available, and why.
    pub refused: Vec<Problem>,
}

/// Why one module was not copied or registered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// The module, as `path@version`.
    pub module: String,
    /// What went wrong.
    pub reason: String,
}

/// What the sync writes to.
pub struct SyncTargets<'a> {
    /// The forge's registry.
    pub registry: &'a RegistryStore,
    /// The actor client, for registering packages with the forge actor.
    pub client: &'a Client,
    /// Extension packages and their runtime.
    pub extensions: &'a Extensions,
}

/// Copies every module `deployment` lists into the registry and registers the packages in
/// them. It never fails as a whole: each module's outcome is in the report.
pub async fn sync_modules(deployment: &Deployment, targets: &SyncTargets<'_>) -> SyncReport {
    targets.registry.list(
        deployment
            .modules
            .iter()
            .map(|m| (m.path.as_str(), m.version.as_str())),
    );
    targets.extensions.begin_sync();
    let mut report = SyncReport::default();
    for module in &deployment.modules {
        let name = module.to_string();
        let problem = |reason: String| Problem {
            module: name.clone(),
            reason,
        };
        match deployment.source_for(&module.path) {
            Some(source) => match copy(targets.registry, source, module).await {
                Ok(CopyOutcome::Copied) => report.copied.push(name.clone()),
                Ok(CopyOutcome::Unchanged) => report.unchanged.push(name.clone()),
                Ok(CopyOutcome::Mismatched { held, source }) => {
                    report.mismatched.push(problem(format!(
                        "the source serves {source}, but the registry holds {held}; the held copy is kept"
                    )));
                }
                Err(error) => report.unreachable.push(problem(format!("{error:#}"))),
            },
            None => report
                .unreachable
                .push(problem("no source serves it".to_owned())),
        }
        match register(targets, module).await {
            Ok(Some(package)) => report.registered.push(package),
            Ok(None) => {}
            Err(Registration::NotHeld) => {}
            Err(Registration::Refused(reason)) => report.refused.push(problem(reason)),
            Err(Registration::Mismatched(reason)) => report.mismatched.push(problem(reason)),
        }
    }
    targets
        .extensions
        .set_available(report.registered.iter().cloned());
    report
}

enum CopyOutcome {
    Copied,
    Unchanged,
    Mismatched { held: String, source: String },
}

fn source_client(source: &Source) -> OciClient {
    let protocol = if source.insecure {
        ClientProtocol::HttpsExcept(vec![source.host.clone()])
    } else {
        ClientProtocol::Https
    };
    OciClient::new(ClientConfig {
        protocol,
        ..ClientConfig::default()
    })
}

/// Copies `module` from `source` unless the registry holds it already.
async fn copy(
    registry: &RegistryStore,
    source: &Source,
    module: &ModuleVersion,
) -> Result<CopyOutcome> {
    let held = registry.manifest(&module.path, &module.version).await?;
    let client = source_client(source);
    let reference = Reference::with_tag(
        source.host.clone(),
        source.repository(&module.path),
        module.version.clone(),
    );
    let auth = RegistryAuth::Anonymous;
    let (manifest, digest) = client
        .pull_manifest_raw(&reference, &auth, &[OCI_IMAGE_MEDIA_TYPE])
        .await
        .with_context(|| format!("pull the manifest of {reference}"))?;
    if digest_of(&manifest) != digest {
        bail!("the source's manifest does not have its digest {digest}");
    }
    if let Some((_, held)) = held {
        return Ok(if held == digest {
            CopyOutcome::Unchanged
        } else {
            CopyOutcome::Mismatched {
                held,
                source: digest,
            }
        });
    }
    let parsed: ImageManifest =
        serde_json::from_slice(&manifest).context("the manifest is not an OCI image manifest")?;
    let blobs = std::iter::once(parsed.config()).chain(parsed.layers().iter());
    for descriptor in blobs {
        let digest = descriptor.digest().to_string();
        if registry.blob_size(&digest).await?.is_some() {
            continue;
        }
        if descriptor.size() > MODULE_MAX_BYTES {
            bail!(
                "blob {digest} of {reference} is {} bytes, more than {MODULE_MAX_BYTES}",
                descriptor.size()
            );
        }
        let mut bytes = Capped::new(MODULE_MAX_BYTES);
        client
            .pull_blob(&reference, digest.as_str(), &mut bytes)
            .await
            .with_context(|| format!("pull blob {digest} of {reference}"))?;
        registry
            .put_blob(&digest, Bytes::from(bytes.into_inner()))
            .await?;
    }
    registry
        .put_manifest(&module.path, &module.version, manifest)
        .await?;
    Ok(CopyOutcome::Copied)
}

/// A buffer that refuses to grow past a limit, for pulling a blob whose source may send more
/// than its descriptor says.
struct Capped {
    bytes: Vec<u8>,
    limit: u64,
}

impl Capped {
    fn new(limit: u64) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl tokio::io::AsyncWrite for Capped {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let total = self.bytes.len().saturating_add(buf.len());
        if u64::try_from(total).map_or(true, |total| total > self.limit) {
            return std::task::Poll::Ready(Err(std::io::Error::other(format!(
                "the blob holds more than {} bytes",
                self.limit
            ))));
        }
        self.bytes.extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

enum Registration {
    /// The registry does not hold the module.
    NotHeld,
    Refused(String),
    Mismatched(String),
}

/// Registers the package in the registry's copy of `module`, if it holds one: returns its
/// `id@version`, or `None` for a module that is not an extension.
async fn register(
    targets: &SyncTargets<'_>,
    module: &ModuleVersion,
) -> Result<Option<String>, Registration> {
    let refused = |error: anyhow::Error| Registration::Refused(format!("{error:#}"));
    let Some((manifest, _)) = targets
        .registry
        .manifest(&module.path, &module.version)
        .await
        .map_err(refused)?
    else {
        return Err(Registration::NotHeld);
    };
    let files = module_files(targets.registry, &manifest)
        .await
        .map_err(refused)?;
    if !files.contains_key("tartan.json") {
        return Ok(None);
    }
    let package = ExtensionPackage::from_module_files(&files)
        .map_err(|error| Registration::Refused(error.to_string()))?;
    let expected = format!("v{}", package.manifest.version);
    if expected != module.version {
        return Err(Registration::Refused(format!(
            "tartan.json has version {}, but the module is {}",
            package.manifest.version, module.version
        )));
    }
    // At startup the sync runs while the actors are still coming up, so a forge actor that is
    // not ready yet is waited for rather than leaving the package unavailable until the next
    // start.
    let mut delay = REGISTER_FIRST_DELAY;
    loop {
        match register_package(targets.client, targets.extensions, &package).await {
            Ok(info) => return Ok(Some(format!("{}@{}", info.id, info.version))),
            Err(RegisterError::Conflict(reason)) => return Err(Registration::Mismatched(reason)),
            Err(RegisterError::Unavailable(error)) if delay <= REGISTER_LAST_DELAY => {
                tracing::info!(module = %module, error = format!("{error:#}"), "registering again shortly");
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            Err(error) => return Err(Registration::Refused(error.to_string())),
        }
    }
}

/// The wait before registering a package again after the forge was unavailable; it doubles up
/// to [`REGISTER_LAST_DELAY`], about two minutes in all.
const REGISTER_FIRST_DELAY: std::time::Duration = std::time::Duration::from_secs(1);
const REGISTER_LAST_DELAY: std::time::Duration = std::time::Duration::from_secs(64);

/// The files of a module's zip, by path.
async fn module_files(
    registry: &RegistryStore,
    manifest: &[u8],
) -> Result<BTreeMap<String, Vec<u8>>> {
    let parsed: ImageManifest =
        serde_json::from_slice(manifest).context("the held manifest is not an OCI manifest")?;
    let layer = parsed
        .layers()
        .iter()
        .find(|layer| layer.media_type().to_string() == MODULE_ZIP)
        .ok_or_else(|| anyhow!("the module has no {MODULE_ZIP} layer"))?;
    let digest = layer.digest().to_string();
    let zip = registry
        .blob(&digest)
        .await?
        .ok_or_else(|| anyhow!("the registry is missing blob {digest}"))?;
    tokio::task::spawn_blocking(move || unzip(&zip)).await?
}

fn unzip(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("the module zip")?;
    let mut files = BTreeMap::new();
    let mut total: u64 = 0;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        if file.is_dir() {
            continue;
        }
        let Some(path) = file.enclosed_name() else {
            bail!("the module zip has an unsafe path {:?}", file.name());
        };
        let path = path
            .to_str()
            .ok_or_else(|| anyhow!("a module path is not UTF-8"))?
            .replace('\\', "/");
        // An entry may hold more than it declares, so what is read counts, not the declared
        // size, and no entry may read past what the limit leaves.
        let remaining = MODULE_MAX_BYTES - total;
        let mut contents = Vec::new();
        (&mut file).take(remaining + 1).read_to_end(&mut contents)?;
        total = total.saturating_add(contents.len() as u64);
        if total > MODULE_MAX_BYTES {
            bail!("the module holds more than {MODULE_MAX_BYTES} bytes");
        }
        files.insert(path, contents);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tokio::io::AsyncWriteExt;

    use super::*;

    fn zip_of(files: &[(&str, usize)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, size) in files {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("start");
            writer.write_all(&vec![b'a'; *size]).expect("write");
        }
        writer.finish().expect("finish").into_inner()
    }

    #[test]
    fn unzips_within_the_limit() {
        let files = unzip(&zip_of(&[("tartan.json", 2), ("a/b.cue", 3)])).expect("unzips");
        assert_eq!(files["a/b.cue"].len(), 3);
        let half = usize::try_from(MODULE_MAX_BYTES / 2).expect("fits");
        assert!(unzip(&zip_of(&[("a", half), ("b", half), ("c", 1)])).is_err());
    }

    #[tokio::test]
    async fn a_capped_buffer_refuses_more_than_its_limit() {
        let mut buffer = Capped::new(4);
        buffer.write_all(b"abcd").await.expect("fits");
        assert!(buffer.write_all(b"e").await.is_err());
        assert_eq!(buffer.into_inner(), b"abcd");
    }
}

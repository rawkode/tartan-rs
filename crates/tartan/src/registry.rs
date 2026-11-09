//! The forge's CUE registry: the read half of the OCI Distribution API at `/v2/`, served from
//! the object store, so `CUE_REGISTRY=<forge>` resolves the modules the forge makes available
//! for the `cue` command and for the forge's own evaluations alike.
//!
//! CUE needs only `GET /v2/`, `GET`/`HEAD /v2/<name>/manifests/<reference>`,
//! `GET`/`HEAD /v2/<name>/blobs/<digest>` and `GET /v2/<name>/tags/list`. The only writer is the
//! forge's module sync ([`crate::sync`]), so every other method answers 405 and there are no
//! upload sessions or garbage collection. Modules are public to anyone who can reach the
//! gateway: they hold schemas and extension packages, never repository data.
//!
//! The registry serves only the module versions the deployment lists ([`RegistryStore::list`]):
//! a version dropped from the list stays in the store but is unknown here, so `cue` and the
//! forge's own evaluations fail to resolve it alike. A manifest by digest and a blob are served
//! under a repository name only when a listed manifest of that repository is or references it.
//!
//! Layout under the store's `registry/` prefix:
//!
//! - `blobs/sha256/<hex>`: every blob, manifests included, by digest;
//! - `manifests/<name>/<tag>`: each tagged manifest's bytes, so a tag listing is a directory
//!   listing.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use anyhow::{Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::GatewayState;

/// The media type of a manifest that does not name its own.
const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
/// The header that carries a manifest's or a blob's digest.
const DIGEST_HEADER: &str = "docker-content-digest";

/// The registry's contents in the object store. Cloning is cheap.
#[derive(Clone)]
pub struct RegistryStore {
    store: Arc<dyn ObjectStore>,
    /// The tags (module versions) the deployment lists, by repository name.
    listed: Arc<RwLock<BTreeMap<String, BTreeSet<String>>>>,
    /// Each held manifest's digest and the digests of the blobs it references, by repository
    /// name and tag. A tag is never retagged, so an entry never changes.
    index: Arc<Mutex<ManifestIndexes>>,
}

/// [`ManifestIndex`]es by repository name and tag.
type ManifestIndexes = HashMap<(String, String), Arc<ManifestIndex>>;

/// What one tagged manifest is and references.
#[derive(Debug)]
struct ManifestIndex {
    digest: String,
    blobs: BTreeSet<String>,
}

static NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$")
        .expect("valid regex")
});
static TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}$").expect("valid regex"));
static DIGEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^sha256:[a-f0-9]{64}$").expect("valid regex"));

/// Whether `name` is a valid OCI repository name.
pub fn valid_name(name: &str) -> bool {
    name.len() <= 255 && NAME.is_match(name)
}

/// Whether `tag` is a valid OCI tag.
pub fn valid_tag(tag: &str) -> bool {
    TAG.is_match(tag)
}

/// The `sha256:<hex>` digest of `bytes`.
pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

impl RegistryStore {
    /// The registry kept under `registry/` in `store`.
    pub fn new(store: Arc<dyn ObjectStore>) -> Self {
        Self {
            store,
            listed: Arc::default(),
            index: Arc::default(),
        }
    }

    /// Serves `modules` (`(path, version)` pairs) and no others.
    pub fn list<'a>(&self, modules: impl IntoIterator<Item = (&'a str, &'a str)>) {
        let mut listed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (path, version) in modules {
            listed
                .entry(path.to_owned())
                .or_default()
                .insert(version.to_owned());
        }
        *self.listed.write().unwrap_or_else(|e| e.into_inner()) = listed;
    }

    /// The listed tags of repository `name`.
    fn listed_tags(&self, name: &str) -> BTreeSet<String> {
        self.listed
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// What the held manifest tagged `tag` in `name` is and references.
    async fn manifest_index(&self, name: &str, tag: &str) -> Result<Option<Arc<ManifestIndex>>> {
        let key = (name.to_owned(), tag.to_owned());
        if let Some(found) = self
            .index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return Ok(Some(found.clone()));
        }
        let Some((bytes, digest)) = self.manifest(name, tag).await? else {
            return Ok(None);
        };
        let mut blobs = BTreeSet::new();
        if let Ok(manifest) = serde_json::from_slice::<oci_spec::image::ImageManifest>(&bytes) {
            blobs.insert(manifest.config().digest().to_string());
            blobs.extend(manifest.layers().iter().map(|l| l.digest().to_string()));
        }
        let found = Arc::new(ManifestIndex { digest, blobs });
        self.index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, found.clone());
        Ok(Some(found))
    }

    /// Whether a listed manifest of `name` has `digest` (`as_manifest`) or references it as a
    /// blob.
    async fn serves(&self, name: &str, digest: &str, as_manifest: bool) -> Result<bool> {
        for tag in self.listed_tags(name) {
            if let Some(index) = self.manifest_index(name, &tag).await? {
                let found = if as_manifest {
                    index.digest == digest
                } else {
                    index.blobs.contains(digest)
                };
                if found {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn blob_path(digest: &str) -> Option<Path> {
        let hex = digest.strip_prefix("sha256:")?;
        DIGEST
            .is_match(digest)
            .then(|| Path::from(format!("registry/blobs/sha256/{hex}")))
    }

    fn tag_path(name: &str, tag: &str) -> Option<Path> {
        (valid_name(name) && valid_tag(tag))
            .then(|| Path::from(format!("registry/manifests/{name}/{tag}")))
    }

    async fn read(&self, path: &Path) -> Result<Option<Bytes>> {
        match self.store.get(path).await {
            Ok(result) => Ok(Some(result.bytes().await?)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(error).with_context(|| format!("read {path}")),
        }
    }

    /// The blob `digest`, if the registry holds it.
    pub async fn blob(&self, digest: &str) -> Result<Option<Bytes>> {
        match Self::blob_path(digest) {
            Some(path) => self.read(&path).await,
            None => Ok(None),
        }
    }

    /// The size of blob `digest`, if the registry holds it.
    pub async fn blob_size(&self, digest: &str) -> Result<Option<u64>> {
        let Some(path) = Self::blob_path(digest) else {
            return Ok(None);
        };
        match self.store.head(&path).await {
            Ok(meta) => Ok(Some(meta.size)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(error).with_context(|| format!("head {path}")),
        }
    }

    /// Stores `bytes` as a blob, after checking they have `digest`.
    pub async fn put_blob(&self, digest: &str, bytes: Bytes) -> Result<()> {
        let path = Self::blob_path(digest).with_context(|| format!("digest {digest:?}"))?;
        anyhow::ensure!(
            digest_of(&bytes) == digest,
            "the blob's contents do not have digest {digest}"
        );
        self.store
            .put(&path, PutPayload::from(bytes))
            .await
            .with_context(|| format!("store {path}"))?;
        Ok(())
    }

    /// The manifest tagged `tag` in repository `name`, and its digest.
    pub async fn manifest(&self, name: &str, tag: &str) -> Result<Option<(Bytes, String)>> {
        let Some(path) = Self::tag_path(name, tag) else {
            return Ok(None);
        };
        Ok(self.read(&path).await?.map(|bytes| {
            let digest = digest_of(&bytes);
            (bytes, digest)
        }))
    }

    /// Tags `manifest` as `tag` in repository `name`. The blobs it references are stored first,
    /// by the caller: a tag is only written once everything it names is present.
    pub async fn put_manifest(&self, name: &str, tag: &str, manifest: Bytes) -> Result<String> {
        let path = Self::tag_path(name, tag).with_context(|| format!("{name}:{tag}"))?;
        let digest = digest_of(&manifest);
        self.put_blob(&digest, manifest.clone()).await?;
        self.store
            .put(&path, PutPayload::from(manifest))
            .await
            .with_context(|| format!("store {path}"))?;
        Ok(digest)
    }

    /// The tags of repository `name`, sorted.
    pub async fn tags(&self, name: &str) -> Result<Vec<String>> {
        if !valid_name(name) {
            return Ok(Vec::new());
        }
        let prefix = Path::from(format!("registry/manifests/{name}"));
        let listing = self
            .store
            .list_with_delimiter(Some(&prefix))
            .await
            .with_context(|| format!("list {prefix}"))?;
        let mut tags: Vec<String> = listing
            .objects
            .into_iter()
            .filter_map(|meta| meta.location.filename().map(str::to_owned))
            .collect();
        tags.sort();
        Ok(tags)
    }
}

/// The registry's routes.
pub(crate) fn routes() -> axum::Router<GatewayState> {
    axum::Router::new()
        .route("/v2", any(base))
        .route("/v2/", any(base))
        .route("/v2/{*path}", any(resource))
}

/// An OCI error response.
struct RegistryError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl RegistryError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn unsupported() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "UNSUPPORTED",
            "this registry is read only",
        )
    }
}

impl From<anyhow::Error> for RegistryError {
    fn from(error: anyhow::Error) -> Self {
        tracing::error!(error = format!("{error:#}"), "registry read failed");
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "UNAVAILABLE",
            "temporarily unavailable",
        )
    }
}

impl IntoResponse for RegistryError {
    fn into_response(self) -> Response {
        let body = json!({"errors": [{"code": self.code, "message": self.message}]});
        let mut response = (self.status, axum::Json(body)).into_response();
        if self.status == StatusCode::METHOD_NOT_ALLOWED {
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        }
        response
    }
}

fn read_only(method: &Method) -> Result<(), RegistryError> {
    if method == Method::GET || method == Method::HEAD {
        Ok(())
    } else {
        Err(RegistryError::unsupported())
    }
}

async fn base(method: Method) -> Result<Response, RegistryError> {
    read_only(&method)?;
    let mut response = axum::Json(json!({})).into_response();
    response.headers_mut().insert(
        "docker-distribution-api-version",
        HeaderValue::from_static("registry/2.0"),
    );
    Ok(response)
}

#[derive(Debug, Default, Deserialize)]
struct TagPage {
    n: Option<usize>,
    last: Option<String>,
}

async fn resource(
    State(state): State<GatewayState>,
    method: Method,
    UrlPath(path): UrlPath<String>,
    Query(page): Query<TagPage>,
) -> Result<Response, RegistryError> {
    read_only(&method)?;
    let registry = &state.registry;
    let head = method == Method::HEAD;
    match route(&path) {
        Some(Route::Tags(name)) => {
            check_name(name)?;
            tags(registry, name, page).await
        }
        Some(Route::Manifest(name, reference)) => {
            check_name(name)?;
            manifest(registry, name, reference, head).await
        }
        Some(Route::Blob(name, digest)) => {
            check_name(name)?;
            blob(registry, name, digest, head).await
        }
        None => Err(RegistryError::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            format!("/v2/{path} is not a registry resource"),
        )),
    }
}

/// A registry resource: what follows `/v2/`.
#[derive(Debug, PartialEq, Eq)]
enum Route<'a> {
    /// `<name>/tags/list`.
    Tags(&'a str),
    /// `<name>/manifests/<reference>`.
    Manifest(&'a str, &'a str),
    /// `<name>/blobs/<digest>`.
    Blob(&'a str, &'a str),
}

/// Reads a resource path from its end, so a repository name may have `manifests`, `blobs` or
/// `tags` elements of its own: the last element is the reference, the one before it the kind.
fn route(path: &str) -> Option<Route<'_>> {
    if let Some(name) = path.strip_suffix("/tags/list") {
        return Some(Route::Tags(name));
    }
    let (rest, reference) = path.rsplit_once('/')?;
    let (name, kind) = rest.rsplit_once('/')?;
    match kind {
        "manifests" => Some(Route::Manifest(name, reference)),
        "blobs" => Some(Route::Blob(name, reference)),
        _ => None,
    }
}

fn check_name(name: &str) -> Result<(), RegistryError> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(RegistryError::new(
            StatusCode::BAD_REQUEST,
            "NAME_INVALID",
            format!("{name:?} is not a repository name"),
        ))
    }
}

async fn tags(
    registry: &RegistryStore,
    name: &str,
    page: TagPage,
) -> Result<Response, RegistryError> {
    let listed = registry.listed_tags(name);
    let all: Vec<String> = registry
        .tags(name)
        .await?
        .into_iter()
        .filter(|tag| listed.contains(tag))
        .collect();
    if all.is_empty() {
        return Err(RegistryError::new(
            StatusCode::NOT_FOUND,
            "NAME_UNKNOWN",
            format!("repository {name} is not known"),
        ));
    }
    let after: Vec<String> = match &page.last {
        Some(last) => all.into_iter().filter(|tag| tag > last).collect(),
        None => all,
    };
    let limit = page.n.unwrap_or(usize::MAX);
    let tags: Vec<String> = after.iter().take(limit).cloned().collect();
    let list = oci_spec::distribution::TagListBuilder::default()
        .name(name)
        .tags(tags.clone())
        .build()
        .map_err(|error| anyhow::anyhow!("tag list: {error}"))?;
    let mut response = axum::Json(list).into_response();
    if tags.len() < after.len()
        && let Some(last) = tags.last()
    {
        let link = format!("</v2/{name}/tags/list?n={limit}&last={last}>; rel=\"next\"");
        if let Ok(value) = HeaderValue::from_str(&link) {
            response.headers_mut().insert(header::LINK, value);
        }
    }
    Ok(response)
}

async fn manifest(
    registry: &RegistryStore,
    name: &str,
    reference: &str,
    head: bool,
) -> Result<Response, RegistryError> {
    let unknown = || {
        RegistryError::new(
            StatusCode::NOT_FOUND,
            "MANIFEST_UNKNOWN",
            format!("{name}:{reference} is not known"),
        )
    };
    let (bytes, digest) = if reference.starts_with("sha256:") {
        if !registry.serves(name, reference, true).await? {
            return Err(unknown());
        }
        let bytes = registry.blob(reference).await?.ok_or_else(unknown)?;
        (bytes, reference.to_owned())
    } else if valid_tag(reference) {
        if !registry.listed_tags(name).contains(reference) {
            return Err(unknown());
        }
        registry
            .manifest(name, reference)
            .await?
            .ok_or_else(unknown)?
    } else {
        return Err(RegistryError::new(
            StatusCode::BAD_REQUEST,
            "MANIFEST_INVALID",
            format!("{reference:?} is neither a tag nor a digest"),
        ));
    };
    // A manifest names its own media type; the response says the same.
    let media_type = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value["mediaType"].as_str().map(str::to_owned))
        .unwrap_or_else(|| OCI_MANIFEST.to_owned());
    let mut headers = HeaderMap::new();
    insert(&mut headers, header::CONTENT_TYPE.as_str(), &media_type);
    insert(&mut headers, DIGEST_HEADER, &digest);
    insert(
        &mut headers,
        header::CONTENT_LENGTH.as_str(),
        &bytes.len().to_string(),
    );
    let body = if head {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    Ok((StatusCode::OK, headers, body).into_response())
}

async fn blob(
    registry: &RegistryStore,
    name: &str,
    digest: &str,
    head: bool,
) -> Result<Response, RegistryError> {
    if !DIGEST.is_match(digest) {
        return Err(RegistryError::new(
            StatusCode::BAD_REQUEST,
            "DIGEST_INVALID",
            format!("{digest:?} is not a sha256 digest"),
        ));
    }
    let unknown = || {
        RegistryError::new(
            StatusCode::NOT_FOUND,
            "BLOB_UNKNOWN",
            format!("blob {digest} is not known"),
        )
    };
    if !registry.serves(name, digest, false).await? {
        return Err(unknown());
    }
    let size = registry.blob_size(digest).await?.ok_or_else(unknown)?;
    let mut headers = HeaderMap::new();
    insert(
        &mut headers,
        header::CONTENT_TYPE.as_str(),
        "application/octet-stream",
    );
    insert(&mut headers, DIGEST_HEADER, digest);
    insert(
        &mut headers,
        header::CONTENT_LENGTH.as_str(),
        &size.to_string(),
    );
    if head {
        return Ok((StatusCode::OK, headers, Body::empty()).into_response());
    }
    let path = RegistryStore::blob_path(digest).ok_or_else(unknown)?;
    let stream = match registry.store.get(&path).await {
        Ok(result) => result.into_stream(),
        Err(object_store::Error::NotFound { .. }) => return Err(unknown()),
        Err(error) => return Err(anyhow::Error::from(error).into()),
    };
    Ok((StatusCode::OK, headers, Body::from_stream(stream)).into_response())
}

fn insert(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        header::HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        headers.insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use object_store::memory::InMemory;

    use super::*;

    #[tokio::test]
    async fn stores_blobs_manifests_and_tags() {
        let registry = RegistryStore::new(Arc::new(InMemory::new()));
        let blob = Bytes::from_static(b"layer");
        let digest = digest_of(&blob);
        assert!(
            registry
                .put_blob(&digest_of(b"other"), blob.clone())
                .await
                .is_err(),
            "a blob must have its digest"
        );
        registry.put_blob(&digest, blob.clone()).await.expect("put");
        assert_eq!(registry.blob(&digest).await.expect("read"), Some(blob));
        assert_eq!(registry.blob_size(&digest).await.expect("head"), Some(5));

        let manifest = Bytes::from_static(br#"{"mediaType":"x"}"#);
        let held = registry
            .put_manifest("example.com/a", "v0.1.0", manifest.clone())
            .await
            .expect("put");
        registry
            .put_manifest("example.com/a", "v0.2.0", manifest.clone())
            .await
            .expect("put");
        registry
            .put_manifest("example.com/a/b", "v1.0.0", manifest.clone())
            .await
            .expect("put");
        assert_eq!(
            registry
                .manifest("example.com/a", "v0.1.0")
                .await
                .expect("read"),
            Some((manifest.clone(), held.clone()))
        );
        assert_eq!(registry.blob(&held).await.expect("read"), Some(manifest));
        assert_eq!(
            registry.tags("example.com/a").await.expect("list"),
            vec!["v0.1.0", "v0.2.0"],
            "a nested repository's tags are not the parent's"
        );
        assert!(registry.tags("Bad").await.expect("list").is_empty());
        assert_eq!(
            registry.manifest("../x", "v0.1.0").await.expect("read"),
            None
        );
    }

    #[test]
    fn routes_read_from_the_end() {
        assert_eq!(route("a/b/tags/list"), Some(Route::Tags("a/b")));
        assert_eq!(
            route("github.com/acme/manifests/blobs/sha256:00"),
            Some(Route::Blob("github.com/acme/manifests", "sha256:00"))
        );
        assert_eq!(
            route("a/blobs/manifests/v0.1.0"),
            Some(Route::Manifest("a/blobs", "v0.1.0"))
        );
        assert_eq!(route("a/tags/tags/list"), Some(Route::Tags("a/tags")));
        assert_eq!(
            route("a/tags/manifests/list"),
            Some(Route::Manifest("a/tags", "list"))
        );
        assert_eq!(route("a/manifests"), None);
        assert_eq!(route("a/other/v1"), None);
    }

    #[tokio::test]
    async fn serves_only_listed_modules_and_their_own_blobs() {
        let registry = RegistryStore::new(Arc::new(InMemory::new()));
        let layer = Bytes::from_static(b"zip");
        let layer_digest = digest_of(&layer);
        registry.put_blob(&layer_digest, layer).await.expect("put");
        let config = Bytes::from_static(b"{}");
        let config_digest = digest_of(&config);
        registry
            .put_blob(&config_digest, config)
            .await
            .expect("put");
        let manifest = serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST,
            "config": {"mediaType": "application/vnd.cue.module.v1+json", "digest": config_digest, "size": 2},
            "layers": [{"mediaType": "application/zip", "digest": layer_digest, "size": 3}],
        }))
        .expect("encode");
        let manifest_digest = registry
            .put_manifest("example.com/a", "v0.1.0", Bytes::from(manifest))
            .await
            .expect("put");
        let blob_of = |name: &'static str, digest: String| {
            let registry = registry.clone();
            async move { registry.serves(name, &digest, false).await.expect("read") }
        };
        // Not listed: nothing is served.
        assert!(!blob_of("example.com/a", layer_digest.clone()).await);
        registry.list([("example.com/a", "v0.1.0")]);
        assert!(blob_of("example.com/a", layer_digest.clone()).await);
        assert!(blob_of("example.com/a", config_digest.clone()).await);
        assert!(!blob_of("example.com/b", layer_digest.clone()).await);
        assert!(
            registry
                .serves("example.com/a", &manifest_digest, true)
                .await
                .expect("read")
        );
        assert!(
            !registry
                .serves("example.com/a", &layer_digest, true)
                .await
                .expect("read"),
            "a layer is not a manifest"
        );
        registry.list([]);
        assert!(!blob_of("example.com/a", layer_digest).await);
    }

    #[test]
    fn validates_names_tags_and_digests() {
        assert!(valid_name("github.com/rawkode/tartan-ext/issues"));
        assert!(valid_name("tartan.test/rawkode/fixture_org"));
        assert!(!valid_name("github.com//x"));
        assert!(!valid_name("../x"));
        assert!(!valid_name("UPPER/x"));
        assert!(valid_tag("v0.6.0"));
        assert!(!valid_tag("v0.6.0/../x"));
        assert!(RegistryStore::blob_path("sha256:abc").is_none());
    }
}

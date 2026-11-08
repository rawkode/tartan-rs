//! The extension API: publishing packages, and listing and calling installations. Installations
//! come only from the `tartan.cue` committed in repositories; there is no install route.

use axum::Json;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use base64::Engine;
use serde::Deserialize;
use serde_json::Value;
use tartan_extensions::{ExtensionPackage, Manifest};
use tartan_forge::{
    Act, CallReply, CallTool, InstallationInfo, InstallationQuery, ListInstallations, ListPackages,
    OrgInfo, PackageInfo, PackageQuery, PublishPackage, Render, RepoInfo, extension_handle,
    forge_handle, org_handle, repo_handle,
};

use crate::ApiError;
use crate::gateway::{GatewayState, resolve_org, resolve_repo};

/// The largest package upload, base64 included.
const PACKAGE_UPLOAD_MAX: usize = 32 << 20;

/// The principal every API call acts as, until the gateway has identities.
const VIEWER: &str = "admin";

/// The extension routes, relative to `/-/api`.
pub(crate) fn routes() -> axum::Router<GatewayState> {
    axum::Router::new()
        .route(
            "/packages",
            post(publish_package)
                .get(list_packages)
                .layer(DefaultBodyLimit::max(PACKAGE_UPLOAD_MAX)),
        )
        .route("/orgs/{org}/extensions", get(list_on_org))
        .route(
            "/orgs/{org}/extensions/{extension}/{call}/{name}",
            post(call_on_org),
        )
        .route("/orgs/{org}/repos/{repo}/extensions", get(list_on_repo))
        .route(
            "/orgs/{org}/repos/{repo}/extensions/{extension}/{call}/{name}",
            post(call_on_repo),
        )
}

#[derive(Debug, Deserialize)]
struct MigrationUpload {
    path: String,
    sql: String,
}

#[derive(Debug, Deserialize)]
struct PackageUpload {
    manifest: Value,
    /// The component, base64-encoded.
    component: String,
    #[serde(default)]
    migrations: Vec<MigrationUpload>,
}

async fn publish_package(
    State(state): State<GatewayState>,
    Json(upload): Json<PackageUpload>,
) -> Result<(StatusCode, Json<PackageInfo>), ApiError> {
    let bad = |message: String| ApiError::BadRequest(message);
    let manifest_bytes = serde_json::to_vec(&upload.manifest).map_err(|e| bad(e.to_string()))?;
    let manifest = Manifest::parse(&manifest_bytes).map_err(|e| bad(e.to_string()))?;
    let component = base64::engine::general_purpose::STANDARD
        .decode(upload.component.as_bytes())
        .map_err(|e| bad(format!("component: {e}")))?;
    let paths: Vec<&str> = upload.migrations.iter().map(|m| m.path.as_str()).collect();
    if paths != manifest.storage.migrations {
        return Err(bad(format!(
            "migrations {paths:?} do not match the manifest's {:?}",
            manifest.storage.migrations
        )));
    }
    let package = ExtensionPackage {
        manifest,
        component,
        migrations: upload
            .migrations
            .into_iter()
            .map(|m| (m.path, m.sql))
            .collect(),
    };
    let digest = package.digest();
    let forge = forge_handle(&state.client)?;
    let existing = forge
        .send(PackageQuery {
            id: package.manifest.id.clone(),
            version: package.manifest.version.clone(),
        })
        .await?;
    match existing {
        Some(found) if found.digest == digest => return Ok((StatusCode::OK, Json(found))),
        Some(found) => {
            return Err(ApiError::Conflict(format!(
                "{}@{} is already published with other contents",
                found.id, found.version
            )));
        }
        None => {}
    }
    state
        .extensions
        .store(&package)
        .await
        .map_err(|error| bad(format!("{error:#}")))?;
    let info = forge
        .send(PublishPackage {
            package: PackageInfo {
                id: package.manifest.id.clone(),
                version: package.manifest.version.clone(),
                digest,
                manifest: package.manifest.raw.clone(),
            },
        })
        .await?;
    Ok((StatusCode::CREATED, Json(info)))
}

async fn list_packages(
    State(state): State<GatewayState>,
) -> Result<Json<Vec<PackageInfo>>, ApiError> {
    Ok(Json(
        forge_handle(&state.client)?.send(ListPackages {}).await?,
    ))
}

/// The org or repository an installation is on.
enum Target {
    Org(OrgInfo),
    Repo(RepoInfo),
}

impl Target {
    async fn org(state: &GatewayState, org: &str) -> Result<Self, ApiError> {
        Ok(Self::Org(resolve_org(&state.client, org).await?))
    }

    async fn repo(state: &GatewayState, org: &str, repo: &str) -> Result<Self, ApiError> {
        Ok(Self::Repo(resolve_repo(&state.client, org, repo).await?))
    }

    fn pool(&self) -> Option<String> {
        match self {
            Self::Org(info) => info.pool.clone(),
            Self::Repo(info) => info.pool.clone(),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Org(info) => info.slug.to_string(),
            Self::Repo(info) => format!("{}/{}", info.org_slug, info.slug),
        }
    }

    async fn list(&self, state: &GatewayState) -> Result<Vec<InstallationInfo>, ApiError> {
        Ok(match self {
            Self::Org(info) => {
                org_handle(&state.client, &info.id, info.pool.clone())?
                    .send(ListInstallations {})
                    .await?
            }
            Self::Repo(info) => {
                repo_handle(&state.client, &info.id, info.pool.clone())?
                    .send(ListInstallations {})
                    .await?
            }
        })
    }

    async fn installation(
        &self,
        state: &GatewayState,
        extension: &str,
    ) -> Result<InstallationInfo, ApiError> {
        let query = InstallationQuery {
            extension_id: extension.to_owned(),
        };
        let found = match self {
            Self::Org(info) => {
                org_handle(&state.client, &info.id, info.pool.clone())?
                    .send(query)
                    .await?
            }
            Self::Repo(info) => {
                repo_handle(&state.client, &info.id, info.pool.clone())?
                    .send(query)
                    .await?
            }
        };
        found.ok_or_else(|| {
            ApiError::NotFound(format!("extension {extension} on {}", self.describe()))
        })
    }
}

async fn list_on_org(
    State(state): State<GatewayState>,
    Path(org): Path<String>,
) -> Result<Json<Vec<InstallationInfo>>, ApiError> {
    Ok(Json(Target::org(&state, &org).await?.list(&state).await?))
}

async fn list_on_repo(
    State(state): State<GatewayState>,
    Path((org, repo)): Path<(String, String)>,
) -> Result<Json<Vec<InstallationInfo>>, ApiError> {
    Ok(Json(
        Target::repo(&state, &org, &repo)
            .await?
            .list(&state)
            .await?,
    ))
}

async fn call_on_org(
    State(state): State<GatewayState>,
    Path((org, extension, call, name)): Path<(String, String, String, String)>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let target = Target::org(&state, &org).await?;
    call_extension(&state, target, &extension, &call, name, input).await
}

async fn call_on_repo(
    State(state): State<GatewayState>,
    Path((org, repo, extension, call, name)): Path<(String, String, String, String, String)>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let target = Target::repo(&state, &org, &repo).await?;
    call_extension(&state, target, &extension, &call, name, input).await
}

/// Calls an installation: `call` is `tools`, `render` or `actions`, and `name` the tool, slot
/// or action.
async fn call_extension(
    state: &GatewayState,
    target: Target,
    extension: &str,
    call: &str,
    name: String,
    input: Value,
) -> Result<Json<Value>, ApiError> {
    let installation = target.installation(state, extension).await?;
    let handle = extension_handle(&state.client, &installation.installation_id, target.pool())?;
    let viewer = VIEWER.to_owned();
    let reply = match call {
        "tools" => {
            handle
                .send(CallTool {
                    tool: name,
                    args: input,
                    viewer,
                })
                .await?
        }
        "render" => {
            handle
                .send(Render {
                    slot: name,
                    props: input,
                    viewer,
                })
                .await?
        }
        "actions" => {
            handle
                .send(Act {
                    action: name,
                    payload: input,
                    viewer,
                })
                .await?
        }
        other => {
            return Err(ApiError::NotFound(format!(
                "{other} of extension {extension}"
            )));
        }
    };
    match reply {
        CallReply::Ok(value) => Ok(Json(value)),
        CallReply::Failed { kind, message } => Err(ApiError::Extension { kind, message }),
    }
}

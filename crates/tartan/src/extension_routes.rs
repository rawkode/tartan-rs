//! The extension API: publishing packages, installing them on repositories, and calling an
//! installation's tools, slots and actions.

use axum::Json;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use tartan_extensions::{ExtensionPackage, Manifest};
use tartan_forge::{
    Act, CallReply, CallTool, Install, InstallationInfo, InstallationQuery, ListInstallations,
    ListPackages, PackageInfo, PackageQuery, PublishPackage, Render, extension_handle,
    forge_handle, repo_handle,
};

use crate::ApiError;
use crate::gateway::{GatewayState, resolve_repo};

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
        .route(
            "/orgs/{org}/repos/{repo}/extensions",
            post(install).get(list_installations),
        )
        .route(
            "/orgs/{org}/repos/{repo}/extensions/{extension}/tools/{tool}",
            post(call_tool),
        )
        .route(
            "/orgs/{org}/repos/{repo}/extensions/{extension}/render/{slot}",
            post(render),
        )
        .route(
            "/orgs/{org}/repos/{repo}/extensions/{extension}/actions/{action}",
            post(act),
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

#[derive(Debug, Deserialize)]
struct InstallBody {
    package: String,
    version: String,
    #[serde(default = "empty_object")]
    config: Value,
}

fn empty_object() -> Value {
    json!({})
}

async fn install(
    State(state): State<GatewayState>,
    Path((org, repo)): Path<(String, String)>,
    Json(body): Json<InstallBody>,
) -> Result<(StatusCode, Json<InstallationInfo>), ApiError> {
    let found = forge_handle(&state.client)?
        .send(PackageQuery {
            id: body.package.clone(),
            version: body.version.clone(),
        })
        .await?;
    let Some(package) = found else {
        return Err(ApiError::NotFound(format!(
            "package {}@{}",
            body.package, body.version
        )));
    };
    if package.manifest["storage"]["scope"] != "repo" {
        return Err(ApiError::BadRequest(format!(
            "{} is not installed per repository",
            package.id
        )));
    }
    let info = resolve_repo(&state.client, &org, &repo).await?;
    let installation = repo_handle(&state.client, &info.id, info.pool)?
        .send(Install {
            extension_id: body.package,
            version: body.version,
            config: body.config,
        })
        .await?;
    Ok((StatusCode::CREATED, Json(installation)))
}

async fn list_installations(
    State(state): State<GatewayState>,
    Path((org, repo)): Path<(String, String)>,
) -> Result<Json<Vec<InstallationInfo>>, ApiError> {
    let info = resolve_repo(&state.client, &org, &repo).await?;
    let installations = repo_handle(&state.client, &info.id, info.pool)?
        .send(ListInstallations {})
        .await?;
    Ok(Json(installations))
}

async fn call_tool(
    State(state): State<GatewayState>,
    Path((org, repo, extension, tool)): Path<(String, String, String, String)>,
    Json(args): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let (installation, pool) = installation(&state, &org, &repo, &extension).await?;
    let reply = extension_handle(&state.client, &installation.installation_id, pool)?
        .send(CallTool {
            tool,
            args,
            viewer: VIEWER.to_owned(),
        })
        .await?;
    into_response(reply)
}

async fn render(
    State(state): State<GatewayState>,
    Path((org, repo, extension, slot)): Path<(String, String, String, String)>,
    Json(props): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let (installation, pool) = installation(&state, &org, &repo, &extension).await?;
    let reply = extension_handle(&state.client, &installation.installation_id, pool)?
        .send(Render {
            slot,
            props,
            viewer: VIEWER.to_owned(),
        })
        .await?;
    into_response(reply)
}

async fn act(
    State(state): State<GatewayState>,
    Path((org, repo, extension, action)): Path<(String, String, String, String)>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let (installation, pool) = installation(&state, &org, &repo, &extension).await?;
    let reply = extension_handle(&state.client, &installation.installation_id, pool)?
        .send(Act {
            action,
            payload,
            viewer: VIEWER.to_owned(),
        })
        .await?;
    into_response(reply)
}

/// The installation of `extension` on `org/repo`, and the pool its repository runs in.
async fn installation(
    state: &GatewayState,
    org: &str,
    repo: &str,
    extension: &str,
) -> Result<(InstallationInfo, Option<String>), ApiError> {
    let info = resolve_repo(&state.client, org, repo).await?;
    let installation = repo_handle(&state.client, &info.id, info.pool.clone())?
        .send(InstallationQuery {
            extension_id: extension.to_owned(),
        })
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("extension {extension} on {org}/{repo}")))?;
    Ok((installation, info.pool))
}

fn into_response(reply: CallReply) -> Result<Json<Value>, ApiError> {
    match reply {
        CallReply::Ok(value) => Ok(Json(value)),
        CallReply::Failed { kind, message } => Err(ApiError::Extension { kind, message }),
    }
}

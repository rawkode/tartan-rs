//! The extension API: the packages the forge makes available, and listing and calling a
//! repository's installations. Packages come only from the modules the deployment
//! configuration lists, and installations only from the configuration committed in
//! repositories; there is no publish or install route.

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::routing::{get, post};
use serde_json::Value;
use tartan_forge::{
    Act, CallReply, CallTool, InstallationInfo, InstallationQuery, ListInstallations, ListPackages,
    PackageInfo, Render, RepoInfo, extension_handle, forge_handle, repo_handle,
};

use crate::ApiError;
use crate::access::{Principal, RequestPrincipal};
use crate::gateway::{GatewayState, resolve_repo};

/// The extension routes, relative to `/-/api`.
pub(crate) fn routes() -> axum::Router<GatewayState> {
    axum::Router::new()
        .route("/packages", get(list_packages))
        .route("/orgs/{org}/repos/{repo}/extensions", get(list_on_repo))
        .route(
            "/orgs/{org}/repos/{repo}/extensions/{extension}/{call}/{name}",
            post(call_on_repo),
        )
}

async fn list_packages(
    State(state): State<GatewayState>,
) -> Result<Json<Vec<PackageInfo>>, ApiError> {
    Ok(Json(
        forge_handle(&state.client)?.send(ListPackages {}).await?,
    ))
}

async fn list_on_repo(
    State(state): State<GatewayState>,
    Path((org, repo)): Path<(String, String)>,
) -> Result<Json<Vec<InstallationInfo>>, ApiError> {
    let repo = resolve_repo(&state.client, &org, &repo).await?;
    Ok(Json(
        repo_handle(&state.client, &repo.id, repo.pool.clone())?
            .send(ListInstallations {})
            .await?,
    ))
}

async fn call_on_repo(
    State(state): State<GatewayState>,
    Extension(caller): Extension<RequestPrincipal>,
    Path((org, repo, extension, call, name)): Path<(String, String, String, String, String)>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let principal = caller.0.ok_or(ApiError::Unauthorized)?;
    let repo = resolve_repo(&state.client, &org, &repo).await?;
    call_extension(&state, &repo, &extension, &call, name, input, &principal).await
}

/// Calls an installation: `call` is `tools`, `render` or `actions`, and `name` the tool, slot
/// or action.
async fn call_extension(
    state: &GatewayState,
    repo: &RepoInfo,
    extension: &str,
    call: &str,
    name: String,
    input: Value,
    principal: &Principal,
) -> Result<Json<Value>, ApiError> {
    // Someone who may read but not write may call only the tools a manifest declares
    // `access: "read"`, run read-only (design D4, *Rights*).
    let read_only = !principal.rights.write;
    let viewer = principal.caller();
    let installation = repo_handle(&state.client, &repo.id, repo.pool.clone())?
        .send(InstallationQuery {
            extension_id: extension.to_owned(),
        })
        .await?
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "extension {extension} on {}/{}",
                repo.org_slug, repo.slug
            ))
        })?;
    let handle = extension_handle(
        &state.client,
        &installation.installation_id,
        repo.pool.clone(),
    )?;
    let reply = match call {
        "tools" => {
            handle
                .send(CallTool {
                    tool: name,
                    args: input,
                    viewer,
                    read_only,
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

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use rivetkit::client::Client;
use serde::{Deserialize, Serialize};
use tartan_forge::{
    CreateOrg, CreateRepo, EventPattern, ListOrgs, ListRepos, OrgInfo, OrgStatus, OrgStatusQuery,
    Placement, Received, ReceivedEvent, RecordRefEvents, RefEvent, RegisterSubscriber, RepoInfo,
    RepoStatusQuery, ResolveOrg, ResolveRepo, SetPlacement, Slug, SubscriberSpec, forge_handle,
    org_handle, repo_handle, subscriber_handle, valid_placement_target, verify_signature,
};

use crate::{ApiError, RegistryStore};

/// The header carrying the Git host's webhook signature.
const SIGNATURE_HEADER: &str = "x-walgit-signature";

/// What the gateway's handlers share.
#[derive(Clone)]
pub struct GatewayState {
    /// The actor client.
    pub client: Client,
    /// The bearer token the forge API requires.
    pub admin_token: Arc<str>,
    /// The key the Git host signs webhook bodies with.
    pub webhook_secret: Arc<[u8]>,
    /// The forge's CUE registry, served read only at `/v2/`.
    pub registry: RegistryStore,
}

/// The gateway's routes.
pub fn router(state: GatewayState) -> Router {
    let api = Router::new()
        .route("/orgs", post(create_org).get(list_orgs))
        .route("/orgs/{org}", get(org_status))
        .route("/orgs/{org}/repos", post(create_repo).get(list_repos))
        .route("/orgs/{org}/repos/{repo}", get(repo_status))
        .route("/placements", put(set_placement))
        .route("/placements/{*target}", get(placement))
        .route("/subscribers/{name}", put(register_subscriber))
        .route("/subscribers/{name}/events", get(received))
        .merge(crate::extension_routes::routes())
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin));
    Router::new()
        .nest("/-/api", api)
        .route("/-/hooks/git-host", post(git_host_events))
        .route("/-/health", get(|| async { "ok" }))
        .merge(crate::registry::routes())
        .with_state(state)
}

async fn require_admin(
    State(state): State<GatewayState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    match presented {
        Some(token) if constant_time_eq(token.as_bytes(), state.admin_token.as_bytes()) => {
            Ok(next.run(request).await)
        }
        _ => Err(ApiError::Unauthorized),
    }
}

#[derive(Debug, Deserialize)]
struct SlugBody {
    slug: String,
}

async fn create_org(
    State(state): State<GatewayState>,
    Json(body): Json<SlugBody>,
) -> Result<(StatusCode, Json<OrgInfo>), ApiError> {
    let slug = parse_slug(&body.slug)?;
    let info = forge_handle(&state.client)?
        .send(CreateOrg { slug })
        .await?;
    Ok((StatusCode::CREATED, Json(info)))
}

/// An org in the forge's listing.
#[derive(Debug, Serialize)]
struct OrgSummary {
    slug: Slug,
}

async fn list_orgs(State(state): State<GatewayState>) -> Result<Json<Vec<OrgSummary>>, ApiError> {
    let orgs = forge_handle(&state.client)?.send(ListOrgs {}).await?;
    Ok(Json(
        orgs.into_iter()
            .map(|org| OrgSummary { slug: org.slug })
            .collect(),
    ))
}

/// A repository in its org's listing.
#[derive(Debug, Serialize)]
struct RepoSummary {
    slug: Slug,
    /// `org/repo`.
    path: String,
    clone_url: String,
}

async fn list_repos(
    State(state): State<GatewayState>,
    Path(org): Path<String>,
) -> Result<Json<Vec<RepoSummary>>, ApiError> {
    let org = resolve_org(&state.client, &org).await?;
    let repos = org_handle(&state.client, &org.id, org.pool)?
        .send(ListRepos {})
        .await?;
    Ok(Json(
        repos
            .into_iter()
            .map(|repo| RepoSummary {
                path: format!("{}/{}", repo.org_slug, repo.slug),
                slug: repo.slug,
                clone_url: repo.clone_url,
            })
            .collect(),
    ))
}

async fn org_status(
    State(state): State<GatewayState>,
    Path(org): Path<String>,
) -> Result<Json<OrgStatus>, ApiError> {
    let info = resolve_org(&state.client, &org).await?;
    let status = org_handle(&state.client, &info.id, info.pool)?
        .send(OrgStatusQuery {})
        .await?;
    Ok(Json(status))
}

async fn create_repo(
    State(state): State<GatewayState>,
    Path(org): Path<String>,
    Json(body): Json<SlugBody>,
) -> Result<(StatusCode, Json<RepoInfo>), ApiError> {
    let slug = parse_slug(&body.slug)?;
    let org = resolve_org(&state.client, &org).await?;
    let info = org_handle(&state.client, &org.id, org.pool)?
        .send(CreateRepo { slug })
        .await?;
    Ok((StatusCode::CREATED, Json(info)))
}

/// A repository and where its actor runs.
#[derive(Debug, Serialize)]
struct RepoView {
    #[serde(flatten)]
    info: RepoInfo,
    /// The Rivet pool hosting the repository actor.
    hosted_in: Option<String>,
}

async fn repo_status(
    State(state): State<GatewayState>,
    Path((org, repo)): Path<(String, String)>,
) -> Result<Json<RepoView>, ApiError> {
    let info = resolve_repo(&state.client, &org, &repo).await?;
    let status = repo_handle(&state.client, &info.id, info.pool.clone())?
        .send(RepoStatusQuery {})
        .await?;
    Ok(Json(RepoView {
        info,
        hosted_in: status.pool,
    }))
}

#[derive(Debug, Deserialize)]
struct PlacementBody {
    target: String,
    pool: Option<String>,
}

async fn set_placement(
    State(state): State<GatewayState>,
    Json(body): Json<PlacementBody>,
) -> Result<StatusCode, ApiError> {
    check_target(&body.target)?;
    forge_handle(&state.client)?
        .send(SetPlacement {
            target: body.target,
            pool: body.pool,
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn placement(
    State(state): State<GatewayState>,
    Path(target): Path<String>,
) -> Result<Json<Option<String>>, ApiError> {
    check_target(&target)?;
    let pool = forge_handle(&state.client)?
        .send(Placement { target })
        .await?;
    Ok(Json(pool))
}

#[derive(Debug, Deserialize)]
struct SubscriberBody {
    pattern: String,
}

async fn register_subscriber(
    State(state): State<GatewayState>,
    Path(name): Path<String>,
    Json(body): Json<SubscriberBody>,
) -> Result<StatusCode, ApiError> {
    let name = parse_slug(&name)?;
    let pattern: EventPattern = body
        .pattern
        .parse()
        .map_err(|error| ApiError::BadRequest(format!("{error}")))?;
    forge_handle(&state.client)?
        .send(RegisterSubscriber {
            subscriber: SubscriberSpec { name, pattern },
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct Page {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    100
}

async fn received(
    State(state): State<GatewayState>,
    Path(name): Path<String>,
    Query(page): Query<Page>,
) -> Result<Json<Vec<ReceivedEvent>>, ApiError> {
    let name = parse_slug(&name)?;
    let events = subscriber_handle(&state.client, name.as_str())?
        .send(Received {
            after: page.after,
            limit: page.limit,
        })
        .await?;
    Ok(Json(events))
}

/// Ref events from the Git host. A 2xx tells the host to advance its cursor, so failures to
/// record answer 503 and the host retries; events for repositories this forge does not know are
/// skipped.
async fn git_host_events(
    State(state): State<GatewayState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let signature = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    if !verify_signature(&state.webhook_secret, &body, signature) {
        return Err(ApiError::Unauthorized);
    }
    let events: Vec<RefEvent> = serde_json::from_slice(&body)
        .map_err(|error| ApiError::BadRequest(format!("ref events: {error}")))?;

    let mut by_repo: BTreeMap<String, Vec<RefEvent>> = BTreeMap::new();
    for event in events {
        by_repo.entry(event.repo.clone()).or_default().push(event);
    }
    for (path, events) in by_repo {
        let Some((org, repo)) = path.split_once('/') else {
            tracing::warn!(%path, "ref events for a malformed repository path; skipped");
            continue;
        };
        let info = match resolve_repo(&state.client, org, repo).await {
            Ok(info) => info,
            Err(ApiError::NotFound(_) | ApiError::BadRequest(_)) => {
                tracing::warn!(%path, "ref events for a repository the forge does not know; skipped");
                continue;
            }
            Err(error) => return Err(error),
        };
        repo_handle(&state.client, &info.id, info.pool)?
            .send(RecordRefEvents { events })
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn resolve_org(client: &Client, org: &str) -> Result<OrgInfo, ApiError> {
    let slug = parse_slug(org)?;
    forge_handle(client)?
        .send(ResolveOrg { slug })
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("org {org}")))
}

pub(crate) async fn resolve_repo(
    client: &Client,
    org: &str,
    repo: &str,
) -> Result<RepoInfo, ApiError> {
    let slug = parse_slug(repo)?;
    let org_info = resolve_org(client, org).await?;
    org_handle(client, &org_info.id, org_info.pool)?
        .send(ResolveRepo { slug })
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("repository {org}/{repo}")))
}

fn check_target(target: &str) -> Result<(), ApiError> {
    if valid_placement_target(target) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "placement target {target:?} is not org/<org> or repo/<org>/<repo>"
        )))
    }
}

fn parse_slug(text: &str) -> Result<Slug, ApiError> {
    text.parse()
        .map_err(|error| ApiError::BadRequest(format!("{error}")))
}

/// Compares two byte strings in time that depends only on their lengths.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

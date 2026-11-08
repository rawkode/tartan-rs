use std::sync::{Arc, OnceLock};

use anyhow::{Result, anyhow};
use async_trait::async_trait;

use crate::Extensions;

/// The service that hosts Git repositories: storage, the wire protocol and ref events.
#[async_trait]
pub trait GitHost: Send + Sync {
    /// Creates the repository `owner/name`; creating one that exists succeeds.
    async fn create_repo(&self, owner: &str, name: &str) -> Result<()>;

    /// The URL `git clone` takes for `owner/name`.
    fn clone_url(&self, owner: &str, name: &str) -> String;

    /// The entries of directory `path` (`""` for the root) in commit `rev` of `owner/name`, or
    /// `None` if there is no such directory.
    async fn list_dir(
        &self,
        owner: &str,
        name: &str,
        rev: &str,
        path: &str,
    ) -> Result<Option<Vec<tartan_config::DirEntry>>>;

    /// The file at `path` in commit `rev` of `owner/name`, or `None` if there is none.
    async fn read_file(
        &self,
        owner: &str,
        name: &str,
        rev: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>>;
}

/// What the actors of this process reach outside the actor system.
#[derive(Clone)]
pub struct Services {
    /// This forge's id. It keys the forge's singleton actors, so several forges can share one
    /// Rivet namespace.
    pub forge_id: String,
    /// The Git host.
    pub git_host: Arc<dyn GitHost>,
    /// Extension packages and their runtime.
    pub extensions: Arc<Extensions>,
}

// Actors are built by the Rivet runtime, which gives them no constructor arguments, so the
// services they need are installed once per process before the registry serves.
static SERVICES: OnceLock<Services> = OnceLock::new();

/// Installs the services for every actor in this process. Call once, before serving.
pub fn install_services(services: Services) -> Result<()> {
    SERVICES
        .set(services)
        .map_err(|_| anyhow!("services are already installed"))
}

/// The installed services.
pub(crate) fn services() -> Result<&'static Services> {
    SERVICES
        .get()
        .ok_or_else(|| anyhow!("install_services was not called before serving actors"))
}

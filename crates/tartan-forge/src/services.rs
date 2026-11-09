use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Result, anyhow};
use async_trait::async_trait;

use tartan_atproto::resolver::VerifyingResolver;
use tartan_config::deployment::Readers;

use crate::Extensions;
use crate::clock::Clock;

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

    /// Lists the objects the update `hidden..new` of `owner/name` introduced and reads those
    /// `skip` does not exclude (design D8, *Reading objects*): the annotated tags `new` names
    /// and the commit they peel to, then every commit reachable from it but not from `hidden`,
    /// newest first, up to `limit` commits. Fails with [`ObjectNotServed`](crate::ObjectNotServed)
    /// when the host no longer serves `new`.
    async fn read_new_objects(
        &self,
        owner: &str,
        name: &str,
        new: &str,
        hidden: &[String],
        limit: usize,
        skip: &(dyn for<'s> Fn(&'s str) -> bool + Send + Sync),
    ) -> Result<crate::Walk>;

    /// Whether `ancestor` is `head` or one of its ancestors in `owner/name`.
    async fn is_ancestor(
        &self,
        owner: &str,
        name: &str,
        ancestor: &str,
        head: &str,
    ) -> Result<bool>;
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
    /// The forge's clock: [`SystemClock`](crate::SystemClock) in the `tartan` binary.
    pub clock: Arc<dyn Clock>,
    /// The deployment configuration's `owners`, as DIDs. Every runner process reads the same
    /// `TARTAN_CONFIG`, so actors in any pool see the same list.
    pub owners: Vec<String>,
    /// The deployment configuration's `readers`.
    pub readers: Readers,
    /// The identity resolver, over the fetch-safe client (design D9).
    pub resolver: VerifyingResolver,
    /// The caps the identity actor enforces.
    pub identity_limits: IdentityLimits,
    /// The object store, under the forge's `tartan/` prefix: proof bundles are stored at
    /// `proofs/sha256/<hex>.json`, beside `registry/` and `packages/` (design D8, *Store*).
    pub objects: Arc<dyn object_store::ObjectStore>,
    /// `TARTAN_STATE_DIR`: local state that can be rebuilt, such as the Git object cache.
    pub state_dir: PathBuf,
}

/// Caps the identity actor enforces (design D4, *Route classes*). The `tartan` binary always
/// uses [`IdentityLimits::default`], the values of the design; tests lower them so a cap can be
/// reached quickly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityLimits {
    /// The most untracked records one DID may withdraw in advance.
    pub pre_withdrawn_per_did: u64,
    /// The most `pre_withdrawn` rows forge-wide.
    pub pre_withdrawn: u64,
    /// The forge-wide count below which a stranger (no `read` right, not known) may still
    /// withdraw an untracked record.
    pub pre_withdrawn_for_strangers: u64,
}

impl Default for IdentityLimits {
    fn default() -> Self {
        Self {
            pre_withdrawn_per_did: tartan_identity_api::MAX_PRE_WITHDRAWN_PER_DID,
            pre_withdrawn: tartan_identity_api::MAX_PRE_WITHDRAWN,
            pre_withdrawn_for_strangers: tartan_identity_api::MAX_PRE_WITHDRAWN_FOR_STRANGERS,
        }
    }
}

impl Services {
    /// Whether `did` is one of the forge's owners.
    pub fn is_owner(&self, did: &str) -> bool {
        self.owners.iter().any(|owner| owner == did)
    }

    /// Whether `did` may read: an owner, or a DID the `readers` setting admits.
    pub fn can_read(&self, did: &str) -> bool {
        self.is_owner(did) || self.readers.admits(did)
    }
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

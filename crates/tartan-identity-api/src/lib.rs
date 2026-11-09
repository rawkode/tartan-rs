//! The identity actor's public interface.
//!
//! The identity actor is a forge singleton implemented in `tartan-forge`. Its name, its key and
//! the message types of its actions live here, so `tt admin` can call it through the Rivet client
//! without linking the forge.
//!
//! Every message type implements [`IdentityAction`], which names the action and its output. With
//! the `rivetkit` feature, which the forge enables, each also implements rivetkit's `Action` with
//! the same name and output. `tt` builds without the feature and sends the messages with
//! `rivetkit-client`'s untyped `ActorHandle::action(IdentityAction::NAME, args)`.
//!
//! Secrets cross this interface in one direction only: an action that creates a session, a
//! token or a CLI code answers with the secret once, and every other action takes the SHA-256 of
//! a secret, hex-encoded, or the code itself for the one-time redemption, never a stored secret.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The actor name the identity actor is registered under.
pub const ACTOR_NAME: &str = "identity";

/// The prefix of an access token's secret (design D5).
pub const TOKEN_PREFIX: &str = "tartan_pat_";

/// The prefix of a CLI session's secret (design D6).
pub const CLI_SESSION_PREFIX: &str = "tartan_cli_";

/// The most unexpired, unrevoked access tokens one DID may hold.
pub const MAX_TOKENS_PER_DID: usize = 50;

/// The longest access-token lifetime, in days.
pub const MAX_TOKEN_DAYS: u32 = 365;

/// The longest token name, in characters.
pub const MAX_TOKEN_NAME_CHARS: usize = 100;

/// The longest device name a CLI login may send, in characters.
pub const MAX_DEVICE_CHARS: usize = 64;

/// The most live CLI codes one DID may hold.
pub const MAX_CLI_CODES_PER_DID: usize = 20;

/// The most DIDs one push may newly make known as signers; the push path enforces it.
pub const MAX_NEW_SIGNERS_PER_PUSH: usize = 50;

/// The most DIDs known as signers, forge-wide; `KnowDid` enforces it.
pub const MAX_SIGNERS: usize = 5000;

/// The key of the identity actor for the forge `forge_id`: one identity actor per forge.
pub fn identity_key(forge_id: &str) -> Vec<String> {
    vec![forge_id.to_owned()]
}

/// An action of the identity actor: its wire name and the type it answers with.
///
/// This mirrors rivetkit's `Action` trait so callers need not link the rivetkit server SDK.
pub trait IdentityAction: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// What the action answers with.
    type Output: Serialize + DeserializeOwned + Send + 'static;

    /// The action's name on the wire.
    const NAME: &'static str;
}

/// Declares that `$message` is the identity action `$name` answering with `$output`, implementing
/// [`IdentityAction`] and, with the `rivetkit` feature, rivetkit's `Action`. It is used only in this
/// crate, where the feature check applies.
macro_rules! identity_action {
    ($message:ty, $name:literal, $output:ty) => {
        impl crate::IdentityAction for $message {
            type Output = $output;
            const NAME: &'static str = $name;
        }

        #[cfg(feature = "rivetkit")]
        impl ::rivetkit::Action for $message {
            type Output = $output;
            const NAME: &'static str = $name;
        }
    };
}

// People ----------------------------------------------------------------------------------------

/// Why the forge keeps track of a DID (`people.known_as`, design D3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownAs {
    /// Listed in the deployment configuration's `owners`.
    Owner,
    /// Signed in with read rights.
    SignedIn,
    /// Made a signature the forge verified.
    Signer,
    /// Named by a signing rule.
    RuleSigner,
}

impl KnownAs {
    /// The value stored in `known_as`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::SignedIn => "signed_in",
            Self::Signer => "signer",
            Self::RuleSigner => "rule_signer",
        }
    }
}

/// Whether an account status refuses the DID's sessions and tokens: every status but `active`,
/// `throttled` and `desynchronized`, including values this crate does not know (design D10).
pub fn status_is_inactive(status: &str) -> bool {
    !matches!(status, "active" | "throttled" | "desynchronized")
}

/// What the forge knows about a DID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// The DID.
    pub did: String,
    /// The verified handle, when one is known.
    pub handle: Option<String>,
    /// The PDS named by the DID's verified document.
    pub pds: Option<String>,
    /// The `#atproto` key as a `did:key`.
    pub atproto_key: Option<String>,
    /// The account status (design D10).
    pub status: String,
    /// When the status was last learned (clock milliseconds), if ever.
    pub status_checked_at: Option<i64>,
    /// Why the forge keeps track of the DID; empty for a DID it only caches.
    pub known_as: Vec<KnownAs>,
    /// When the forge first saw the DID.
    pub first_seen_at: i64,
    /// When the DID last made a signature the forge verified.
    pub last_signed_at: Option<i64>,
}

/// Makes `did` known for `because`, so its identity and account status are watched. Answers
/// whether the DID is known for that reason afterwards: a new signer beyond [`MAX_SIGNERS`] is
/// not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowDid {
    /// The DID.
    pub did: String,
    /// Why.
    pub because: KnownAs,
}

identity_action!(KnowDid, "knowDid", bool);

/// Records a sign-in by `did`. Only a DID with read rights is recorded, as `person.signed_in`,
/// and made known as `signed_in` (design D13, *Who becomes known*); answers whether it was.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedIn {
    /// The DID that signed in.
    pub did: String,
    /// Its verified handle, if any.
    pub handle: Option<String>,
}

identity_action!(SignedIn, "signedIn", bool);

/// Re-learns `did`'s identity and account status from its verified PLC log or did:web
/// document and its PDS (design D10, *Account status*). It takes no other payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshIdentity {
    /// The DID.
    pub did: String,
}

/// What a [`RefreshIdentity`] learned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refreshed {
    /// The stored status after the refresh.
    pub status: String,
    /// The PDS the verified DID document names, when the DID could be resolved.
    pub pds: Option<String>,
    /// Whether the status was learned now; false when a fetch failed and the previous status
    /// was kept.
    pub fresh: bool,
}

identity_action!(RefreshIdentity, "refreshIdentity", Refreshed);

/// What the forge has cached about each of `dids`, in order; `None` for a DID it has no entry
/// for. It never fetches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct People {
    /// The DIDs.
    pub dids: Vec<String>,
}

identity_action!(People, "people", Vec<Option<Person>>);

// Sessions --------------------------------------------------------------------------------------

/// How a session is presented (design D6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// The web UI's cookie.
    Browser,
    /// A `tartan_cli_` bearer secret held by `tt`.
    Cli,
}

impl SessionKind {
    /// The value stored in `sessions.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Cli => "cli",
        }
    }
}

/// Creates a session for `did`. The answer carries the secret once: the cookie value for a
/// browser session, `tartan_cli_…` for a CLI session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSession {
    /// The person.
    pub did: String,
    /// How the session is presented.
    pub kind: SessionKind,
    /// The browser's user agent, or the device name `tt` sent.
    pub device: Option<String>,
}

/// A new session and its secret, which is shown only here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSession {
    /// The session's id (a ULID).
    pub id: String,
    /// The DID it acts as.
    pub did: String,
    /// The secret.
    pub secret: String,
    /// When it expires at the latest (clock milliseconds).
    pub expires_at: i64,
}

identity_action!(CreateSession, "createSession", NewSession);

/// A session as its person sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The id.
    pub id: String,
    /// How it is presented.
    pub kind: SessionKind,
    /// The user agent or device name.
    pub device: Option<String>,
    /// When it was created.
    pub created_at: i64,
    /// When it was last used (written at most once an hour).
    pub last_used_at: i64,
    /// When it expires at the latest.
    pub expires_at: i64,
}

/// The live sessions of `did`, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSessions {
    /// The person.
    pub did: String,
}

identity_action!(ListSessions, "listSessions", Vec<SessionInfo>);

/// Ends session `id` of `did`; answers whether there was one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndSession {
    /// The person.
    pub did: String,
    /// The session.
    pub id: String,
}

identity_action!(EndSession, "endSession", bool);

/// How a credential was presented to the gateway.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    /// The session cookie.
    BrowserSession,
    /// A `tartan_cli_` bearer secret.
    CliSession,
    /// A `tartan_pat_` access token.
    Token,
}

/// Looks up the credential whose secret hashes to `secret_sha256`. Answers nothing for an
/// unknown, expired, idle, revoked or mismatched credential, and for one whose DID's account
/// status is inactive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authenticate {
    /// The SHA-256 of the presented secret, as lowercase hex.
    pub secret_sha256: String,
    /// How it was presented.
    pub kind: CredentialKind,
}

/// A credential the identity actor accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authenticated {
    /// The DID the credential acts as, or on behalf of.
    pub did: String,
    /// The credential.
    pub credential: Credential,
}

/// The credential behind an [`Authenticated`] principal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credential {
    /// A session: the person themself.
    Session {
        /// The session's id.
        id: String,
        /// Browser or CLI.
        kind: SessionKind,
    },
    /// An access token: an agent acting for the person.
    Token {
        /// The token's id.
        id: String,
        /// The token's scopes.
        scopes: Vec<TokenScope>,
        /// The repositories it is limited to, if any.
        repos: Option<Vec<String>>,
    },
}

identity_action!(Authenticate, "authenticate", Option<Authenticated>);

// Access tokens ---------------------------------------------------------------------------------

/// A right an access token may carry (design D5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    /// `GET` routes and extension renders.
    Read,
    /// Extension tool and act calls.
    Write,
    /// Orgs, repositories, placements and subscribers.
    Admin,
}

/// What a new token should be.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRequest {
    /// A name for the person's own reference.
    pub name: String,
    /// Its scopes: a non-empty set.
    pub scopes: Vec<TokenScope>,
    /// `org/repo` or `org/*` entries it is limited to, if any.
    pub repos: Option<Vec<String>>,
    /// Its lifetime, 1 to [`MAX_TOKEN_DAYS`] days.
    pub expires_in_days: u32,
}

/// A new token and its secret, which is shown only here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewToken {
    /// The token as listed.
    pub token: TokenInfo,
    /// The secret: `tartan_pat_` and 43 base64url characters.
    pub secret: String,
}

/// A token as listed: never its secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfo {
    /// The id (a ULID).
    pub id: String,
    /// The DID it acts on behalf of.
    pub did: String,
    /// Its name.
    pub name: String,
    /// Its scopes.
    pub scopes: Vec<TokenScope>,
    /// The repositories it is limited to, if any.
    pub repos: Option<Vec<String>>,
    /// When it was created.
    pub created_at: i64,
    /// When it expires.
    pub expires_at: i64,
    /// When it was last used (written at most once an hour), if ever.
    pub last_used_at: Option<i64>,
    /// When it was revoked, if it was.
    pub revoked_at: Option<i64>,
    /// Who revoked it: a DID, or `operator`.
    pub revoked_by: Option<String>,
}

/// Why a token action was refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum TokenRefusal {
    /// The request is malformed: an unknown scope, a bad repository entry, an expiry out of
    /// range or a bad name.
    Invalid {
        /// What is wrong.
        message: String,
    },
    /// The session that asked is not live.
    NoSession,
    /// The DID already holds [`MAX_TOKENS_PER_DID`] live tokens.
    TooMany,
    /// No such token, or not one the caller may act on.
    NotFound,
    /// The operator path named a DID that is not one of the forge's owners.
    NotOwner {
        /// The DID.
        did: String,
    },
}

impl std::fmt::Display for TokenRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid { message } => write!(f, "{message}"),
            Self::NoSession => write!(f, "tokens are created only from a live session"),
            Self::TooMany => write!(
                f,
                "a DID may hold at most {MAX_TOKENS_PER_DID} unexpired, unrevoked tokens"
            ),
            Self::NotFound => write!(f, "no such token"),
            Self::NotOwner { did } => write!(f, "{did} is not one of this forge's owners"),
        }
    }
}

impl std::error::Error for TokenRefusal {}

/// Creates a token for the person of live session `session_id`. Tokens never create tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateToken {
    /// The browser or CLI session asking.
    pub session_id: String,
    /// The token.
    pub request: TokenRequest,
}

identity_action!(CreateToken, "createToken", Result<NewToken, TokenRefusal>);

/// The tokens of `did`, newest first, revoked and expired ones included.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListTokens {
    /// The person.
    pub did: String,
}

identity_action!(ListTokens, "listTokens", Vec<TokenInfo>);

/// Revokes token `id` on behalf of `by`, who must be the token's DID or an owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeToken {
    /// The DID asking.
    pub by: String,
    /// The token.
    pub id: String,
}

identity_action!(RevokeToken, "revokeToken", Result<(), TokenRefusal>);

/// The operator path (`tt admin token create`): creates a token for owner `did`. Refused for a
/// DID that is not one of the forge's owners.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminCreateToken {
    /// The owner.
    pub did: String,
    /// The token.
    pub request: TokenRequest,
}

identity_action!(AdminCreateToken, "adminCreateToken", Result<NewToken, TokenRefusal>);

/// The operator path (`tt admin token list`): the tokens of owner `did`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminListTokens {
    /// The owner.
    pub did: String,
}

identity_action!(
    AdminListTokens,
    "adminListTokens",
    Result<Vec<TokenInfo>, TokenRefusal>
);

/// The operator path (`tt admin token revoke`): revokes token `id` of owner `did`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminRevokeToken {
    /// The owner.
    pub did: String,
    /// The token.
    pub id: String,
}

identity_action!(AdminRevokeToken, "adminRevokeToken", Result<(), TokenRefusal>);

// The tt login hand-off (design D16) ------------------------------------------------------------

/// Why a CLI hand-off step was refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CliRefusal {
    /// A malformed challenge, device name or verifier.
    Invalid {
        /// What is wrong.
        message: String,
    },
    /// The DID already holds [`MAX_CLI_CODES_PER_DID`] live codes.
    TooMany,
    /// The code is unknown, already used or expired, or the verifier does not match.
    Refused,
}

impl std::fmt::Display for CliRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid { message } => write!(f, "{message}"),
            Self::TooMany => write!(f, "too many pending tt logins; try again in a minute"),
            Self::Refused => write!(f, "the code is unknown, used, expired or not yours"),
        }
    }
}

impl std::error::Error for CliRefusal {}

/// Issues a one-time CLI code for `did`, valid for 60 seconds. The answer is the code, shown
/// only here; the actor stores its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueCliCode {
    /// The person approving.
    pub did: String,
    /// The PKCE S256 challenge `tt` sent: 43 base64url characters.
    pub challenge: String,
    /// The device name `tt` sent, at most [`MAX_DEVICE_CHARS`] characters.
    pub device: String,
    /// The client address the approval came from.
    pub client_ip: String,
}

identity_action!(IssueCliCode, "issueCliCode", Result<String, CliRefusal>);

/// Redeems a CLI code: deletes it whatever happens, requires `S256(verifier)` to equal its
/// challenge, and creates a CLI session whose `tartan_cli_` secret the answer carries once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedeemCliCode {
    /// The code.
    pub code: String,
    /// The PKCE verifier.
    pub verifier: String,
}

identity_action!(RedeemCliCode, "redeemCliCode", Result<NewSession, CliRefusal>);

// Housekeeping ----------------------------------------------------------------------------------

/// The identity actor's scheduled housekeeping (design D3, *Scheduled sweep*): deletes expired
/// rows, refreshes the status of known DIDs not checked for 6 hours, and drops DIDs nothing
/// keeps known any more. Rivet runs it every 15 minutes; tests call it directly.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sweep {}

/// What a [`Sweep`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepReport {
    /// Expired rows deleted.
    pub expired: u64,
    /// DIDs whose identity and status were refreshed.
    pub refreshed: Vec<String>,
    /// `(did, reason)` pairs dropped from `known_as`.
    pub dropped: Vec<(String, KnownAs)>,
}

identity_action!(Sweep, "sweep", SweepReport);

/// The name of the recurring schedule that runs [`Sweep`].
pub const SWEEP_SCHEDULE: &str = "sweep";

/// The names of the identity actor's recurring schedules, for operators and tests.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedules {}

identity_action!(Schedules, "schedules", Vec<String>);

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Ping {
        n: u32,
    }

    identity_action!(Ping, "Ping", u32);

    #[test]
    fn declared_actions_carry_their_name() {
        assert_eq!(<Ping as IdentityAction>::NAME, "Ping");
        #[cfg(feature = "rivetkit")]
        assert_eq!(<Ping as rivetkit::Action>::NAME, "Ping");
        assert_eq!(<Sweep as IdentityAction>::NAME, SWEEP_SCHEDULE);
    }

    #[test]
    fn key_is_the_forge_id() {
        assert_eq!(identity_key("forge"), vec!["forge".to_owned()]);
    }

    #[test]
    fn inactive_statuses_fail_closed() {
        for status in ["active", "throttled", "desynchronized"] {
            assert!(!status_is_inactive(status), "{status}");
        }
        for status in [
            "takendown",
            "suspended",
            "deleted",
            "deactivated",
            "inactive",
            "some-future-status",
        ] {
            assert!(status_is_inactive(status), "{status}");
        }
    }

    #[test]
    fn refusals_travel_as_tagged_json() {
        let refusal: Result<(), TokenRefusal> = Err(TokenRefusal::NotOwner {
            did: "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa".into(),
        });
        let json = serde_json::to_value(&refusal).expect("encode");
        assert_eq!(json["Err"]["reason"], "not_owner");
        let back: Result<(), TokenRefusal> = serde_json::from_value(json).expect("decode");
        assert_eq!(back, refusal);
    }
}

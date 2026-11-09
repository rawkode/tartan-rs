//! `tt`'s command line (design D16). Git's own signing argv never reaches clap: `main` detects
//! it first.

use clap::{Args, Parser, Subcommand, ValueEnum};

/// The compromise procedure, shown by `tt logout --help` and `tt sign revoke --help`.
pub const COMPROMISE: &str = "\
If a signing credential may be compromised: any credential with write access to your \
account's repository can sign Git objects as you, not only tt's session. In this order:
  1. At your PDS, revoke every app password and every app session with repository write \
access (transition:generic, repo:*, or a repo: permission covering \
academy.rawkode.tartan.signature), tt's own included; change the account password if it may \
be known.
  2. Run `tt sign revoke <commit or at:// URI>` for each forged signature. Signed in to the \
forge with `tt login` as the account, this also withdraws it there.
  3. Run `tt sign --login` for a fresh session.
Do not start with `tt logout`: it ends the forge CLI session step 2 uses.";

/// tt, the Tartan command-line interface.
#[derive(Debug, Parser)]
#[command(name = "tt", version, about, long_about = None)]
pub struct Cli {
    /// The command.
    #[command(subcommand)]
    pub command: Command,
}

/// `tt`'s commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in to a forge from this terminal, then set up signing for the same account.
    Login(LoginArgs),
    /// End the forge session and the signing sessions and delete local credentials.
    #[command(after_help = COMPROMISE)]
    Logout(LogoutArgs),
    /// Show the forge session, signing sessions, queued signature records and the last
    /// signing failure.
    Status(StatusArgs),
    /// Manage the forge access tokens of the signed-in person.
    Token(TokenArgs),
    /// Resolve and verify ATProto identities.
    Identity(IdentityArgs),
    /// Sign and verify Git objects with ATProto records; set up Git; manage the queue.
    Sign(Box<SignArgs>),
    /// Operator commands that reach the forge's actors directly.
    Admin(AdminArgs),
}

/// Which OAuth client signing uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ClientChoice {
    /// ATProto's `http://localhost` development client, for a forge not on public HTTPS.
    Localhost,
}

/// `tt login`.
#[derive(Debug, Args)]
pub struct LoginArgs {
    /// The forge's URL (default: `TT_FORGE`, then the default forge).
    #[arg(long)]
    pub forge: Option<String>,
    /// Make this forge the default forge (and, on first run, this DID the default signing DID).
    #[arg(long)]
    pub set_default: bool,
    /// Print the URL to open instead of opening a browser.
    #[arg(long)]
    pub no_open: bool,
    /// Listen on this loopback port (to forward it over SSH).
    #[arg(long)]
    pub port: Option<u16>,
    /// The OAuth client the signing login uses.
    #[arg(long, value_enum)]
    pub client: Option<ClientChoice>,
    /// A development client: the https URL where you host tt's client metadata, for
    /// authorization servers that refuse `--client localhost` (dev-identity builds only; tt
    /// prints the document to host).
    #[arg(long, env = "TT_CLIENT_METADATA_URL", conflicts_with = "client")]
    pub client_metadata_url: Option<String>,
    /// Do not set up signing after signing in.
    #[arg(long)]
    pub no_signing: bool,
    /// Read a `tartan_pat_` access token from standard input instead of signing in.
    #[arg(long)]
    pub with_token: bool,
}

/// `tt logout`.
#[derive(Debug, Args)]
pub struct LogoutArgs {
    /// The forge to sign out of (default: `TT_FORGE`, then the default forge; none is needed
    /// to end signing sessions).
    #[arg(long)]
    pub forge: Option<String>,
    /// End only this DID's signing session (default: every stored signing session).
    #[arg(long)]
    pub did: Option<String>,
}

/// `tt status`.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// The forge to show (default: `TT_FORGE`, then the default forge).
    #[arg(long)]
    pub forge: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `tt token`.
#[derive(Debug, Args)]
pub struct TokenArgs {
    /// The forge (default: `TT_FORGE`, then the default forge).
    #[arg(long, global = true)]
    pub forge: Option<String>,
    /// The token command.
    #[command(subcommand)]
    pub command: TokenCommand,
}

/// `tt token` commands.
#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// Create an access token; its secret is printed once.
    Create(TokenCreateArgs),
    /// List your access tokens.
    List,
    /// Revoke an access token.
    Revoke {
        /// The token's id.
        id: String,
    },
}

/// `tt token create` and `tt admin token create`.
#[derive(Debug, Args)]
pub struct TokenCreateArgs {
    /// A name for your own reference.
    #[arg(long)]
    pub name: String,
    /// Comma-separated scopes: read, write, admin.
    #[arg(long, value_delimiter = ',')]
    pub scopes: Vec<String>,
    /// Comma-separated `org/repo` or `org/*` entries to limit the token to.
    #[arg(long, value_delimiter = ',')]
    pub repos: Option<Vec<String>>,
    /// Lifetime in days (1 to 365).
    #[arg(long)]
    pub expires_in_days: u32,
}

/// `tt identity`.
#[derive(Debug, Args)]
pub struct IdentityArgs {
    /// The identity command.
    #[command(subcommand)]
    pub command: IdentityCommand,
}

/// `tt identity` commands.
#[derive(Debug, Subcommand)]
pub enum IdentityCommand {
    /// Print the DID, verified handle and PDS of a handle or DID.
    Resolve {
        /// A handle (`alice.example`) or a DID.
        who: String,
    },
}

/// `tt sign`.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct SignArgs {
    /// `revoke` or `recheck`.
    #[command(subcommand)]
    pub command: Option<SignCommand>,
    /// Sign in to your PDS for signing.
    #[arg(long, conflicts_with_all = ["setup", "flush"])]
    pub login: bool,
    /// With --login: the handle or DID to sign in as.
    #[arg(long, requires = "login")]
    pub handle: Option<String>,
    /// With --login: the forge whose client metadata the session uses.
    #[arg(long)]
    pub forge: Option<String>,
    /// With --login: the OAuth client.
    #[arg(long, value_enum)]
    pub client: Option<ClientChoice>,
    /// With --login: a development client: the https URL where you host tt's client metadata, for
    /// authorization servers that refuse `--client localhost` (dev-identity builds only; tt
    /// prints the document to host).
    #[arg(long, env = "TT_CLIENT_METADATA_URL", conflicts_with = "client")]
    pub client_metadata_url: Option<String>,
    /// With --login: print the URL instead of opening a browser.
    #[arg(long)]
    pub no_open: bool,
    /// With --login: the loopback port.
    #[arg(long)]
    pub port: Option<u16>,
    /// With --login: make this DID the default signing DID.
    #[arg(long)]
    pub set_default: bool,
    /// Configure Git to sign with tt.
    #[arg(long, conflicts_with = "flush")]
    pub setup: bool,
    /// With --setup: write the user's Git configuration instead of the repository's.
    #[arg(long)]
    pub global: bool,
    /// With --setup: the DID to sign as.
    #[arg(long)]
    pub did: Option<String>,
    /// With --setup: the program path Git runs.
    #[arg(long)]
    pub program: Option<String>,
    /// With --setup: do not sign tags by default.
    #[arg(long)]
    pub no_tags: bool,
    /// With --setup: configure verification only.
    #[arg(long)]
    pub verify_only: bool,
    /// With --setup: install the pre-push hook. With --flush: run as the hook (always exit 0).
    #[arg(long)]
    pub hook: bool,
    /// With --setup --hook: accept a shared or tracked hooks directory.
    #[arg(long)]
    pub hooks_path_ok: bool,
    /// With --setup: undo what --setup wrote.
    #[arg(long)]
    pub undo: bool,
    /// Publish queued signature records.
    #[arg(long)]
    pub flush: bool,
}

/// `tt sign` subcommands.
#[derive(Debug, Subcommand)]
pub enum SignCommand {
    /// Delete a signature record and withdraw it on the signed-in forge.
    #[command(after_help = COMPROMISE)]
    Revoke(RevokeArgs),
    /// Ask the forge to re-check your commit's unverifiable signature.
    Recheck(RecheckArgs),
}

/// `tt sign revoke`.
#[derive(Debug, Args)]
pub struct RevokeArgs {
    /// A signed commit or tag, or the record's `at://` URI.
    pub target: String,
    /// The forge to withdraw it on (default: `TT_FORGE`, then the default forge).
    #[arg(long)]
    pub forge: Option<String>,
    /// The OAuth client for the one-off authorization.
    #[arg(long, value_enum)]
    pub client: Option<ClientChoice>,
    /// A development client: the https URL where you host tt's client metadata, for
    /// authorization servers that refuse `--client localhost` (dev-identity builds only; tt
    /// prints the document to host).
    #[arg(long, env = "TT_CLIENT_METADATA_URL", conflicts_with = "client")]
    pub client_metadata_url: Option<String>,
    /// Print the URL instead of opening a browser.
    #[arg(long)]
    pub no_open: bool,
    /// The loopback port.
    #[arg(long)]
    pub port: Option<u16>,
}

/// `tt sign recheck`.
#[derive(Debug, Args)]
pub struct RecheckArgs {
    /// The commit.
    pub commit: String,
    /// The repository on the forge, as `org/repo`.
    #[arg(long)]
    pub repo: String,
    /// The forge (default: `TT_FORGE`, then the default forge).
    #[arg(long)]
    pub forge: Option<String>,
}

/// `tt admin`.
#[derive(Debug, Args)]
pub struct AdminArgs {
    /// The admin command.
    #[command(subcommand)]
    pub command: AdminCommand,
}

/// `tt admin` commands.
#[derive(Debug, Subcommand)]
pub enum AdminCommand {
    /// Operator access tokens for an owner, through Rivet (`RIVET_ENDPOINT`, `RIVET_TOKEN`,
    /// `RIVET_NAMESPACE`, `TARTAN_FORGE_ID`).
    Token(AdminTokenArgs),
    /// OAuth key material for the forge.
    Oauth(AdminOauthArgs),
}

/// `tt admin token`.
#[derive(Debug, Args)]
pub struct AdminTokenArgs {
    /// The command.
    #[command(subcommand)]
    pub command: AdminTokenCommand,
}

/// `tt admin token` commands.
#[derive(Debug, Subcommand)]
pub enum AdminTokenCommand {
    /// Create a token for an owner; its secret is printed once.
    Create {
        /// The owner's DID.
        #[arg(long)]
        did: String,
        /// The token.
        #[command(flatten)]
        token: TokenCreateArgs,
    },
    /// List an owner's tokens.
    List {
        /// The owner's DID.
        #[arg(long)]
        did: String,
    },
    /// Revoke an owner's token.
    Revoke {
        /// The owner's DID.
        #[arg(long)]
        did: String,
        /// The token's id.
        id: String,
    },
}

/// `tt admin oauth`.
#[derive(Debug, Args)]
pub struct AdminOauthArgs {
    /// The command.
    #[command(subcommand)]
    pub command: AdminOauthCommand,
}

/// `tt admin oauth` commands.
#[derive(Debug, Subcommand)]
pub enum AdminOauthCommand {
    /// Print a fresh ES256 private JWKS for `TARTAN_OAUTH_KEYS_FILE`.
    Keygen,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::*;

    #[test]
    fn the_command_list() {
        Cli::command().debug_assert();
        let names: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|command| command.get_name().to_owned())
            .collect();
        assert_eq!(
            names,
            [
                "login", "logout", "status", "token", "identity", "sign", "admin"
            ]
        );
    }

    #[test]
    fn revoke_help_states_the_procedure() {
        let mut command = Cli::command();
        let sign = command.find_subcommand_mut("sign").expect("sign");
        let revoke = sign.find_subcommand_mut("revoke").expect("revoke");
        let help = revoke.render_long_help().to_string();
        assert!(help.contains("any credential with write access"), "{help}");
        assert!(help.contains("revoke every app password"), "{help}");
        assert!(help.contains("tt sign --login"), "{help}");
        let logout = command.find_subcommand_mut("logout").expect("logout");
        assert!(
            logout
                .render_long_help()
                .to_string()
                .contains("Do not start with `tt logout`")
        );
    }
}

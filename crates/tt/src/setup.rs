//! `tt sign --setup`, `--setup --hook` and `--setup --undo` (design D7, *`tt sign` as Git's
//! signing program* and *Publishing the queue*).
//!
//! Setup writes, with `git config` in the repository (or the user's configuration with
//! `--global`): `gpg.format=x509`, `gpg.x509.program`, `user.signingKey=<DID>`,
//! `commit.gpgSign=true`, `tag.gpgSign=true` (unless `--no-tags`), and adds the DID to
//! `tt.trustedSigner`. `--verify-only` writes only `gpg.x509.program`. It never writes
//! `config.toml`.
//!
//! Undo removes only what is still as `tt` wrote it: `gpg.format` while it is `x509`,
//! `gpg.x509.program` while it names a `tt` program, `user.signingKey` while it is a DID (the
//! only signing keys `tt` uses), `commit.gpgSign` and `tag.gpgSign` while `true` and the setup is
//! still in force, and that DID from `tt.trustedSigner`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::config::Config;
use crate::ctx::Ctx;
use crate::git::{self, Scope};
use crate::oauth::SigningSession;
use crate::state;

/// The marker line of the hook `tt` writes.
pub const HOOK_MARKER: &str = "# tt-sign-hook v1";

/// What a setup is asked to do.
pub struct SetupRequest {
    /// `--global`.
    pub global: bool,
    /// `--did`.
    pub did: Option<String>,
    /// `--program`.
    pub program: Option<String>,
    /// `--no-tags`.
    pub no_tags: bool,
    /// `--verify-only`.
    pub verify_only: bool,
    /// `--hook`.
    pub hook: bool,
    /// `--hooks-path-ok`.
    pub hooks_path_ok: bool,
}

/// The program path Git should run: `tt` when the `tt` on `PATH` is this binary, else this
/// binary's absolute path, or `--program`.
pub fn program_path(explicit: Option<&str>) -> Result<String> {
    if let Some(program) = explicit {
        let path = Path::new(program);
        if !path.is_absolute() {
            bail!("--program must be an absolute path");
        }
        return Ok(program.to_owned());
    }
    let current = std::env::current_exe()
        .context("finding the running tt")?
        .canonicalize()
        .context("finding the running tt")?;
    if let Some(on_path) = find_on_path("tt")
        && on_path.canonicalize().ok().as_deref() == Some(current.as_path())
    {
        return Ok("tt".to_owned());
    }
    current
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("the path of the running tt is not UTF-8"))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// The DID setup writes: `--did`, else the default signing DID, else the only stored session's.
fn setup_did(ctx: &Ctx, explicit: Option<&str>) -> Result<Option<String>> {
    if let Some(did) = explicit {
        tartan_atproto::did::Did::parse(did, ctx.mode)
            .map_err(|error| anyhow!("{did}: {error}"))?;
        return Ok(Some(did.to_owned()));
    }
    let config = Config::load(&ctx.dirs.config)?;
    if let Some(did) = config.file.default_signing_did {
        return Ok(Some(did));
    }
    let index = state::index(&ctx.dirs)?;
    let mut dids = index.signing.keys();
    match (dids.next(), dids.next()) {
        (Some(only), None) => Ok(Some(only.clone())),
        (None, _) => Ok(None),
        (Some(_), Some(_)) => bail!(
            "several signing sessions and no default signing DID; choose one with `tt sign \
             --setup --did <DID>`"
        ),
    }
}

/// Runs the setup. `login` signs in when no session is stored and returns the new DID.
pub async fn setup<F, Fut>(ctx: &Ctx, request: SetupRequest, login: F) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    if request.hook && request.global {
        bail!(
            "hooks are per repository and tt never sets core.hooksPath; run `tt sign --setup \
             --hook` in each repository where offline work is pushed (nothing was changed)"
        );
    }
    let scope = if request.global {
        Scope::Global
    } else {
        Scope::Local
    };
    if scope == Scope::Local {
        git::run(&["rev-parse", "--git-dir"]).map_err(|_| {
            anyhow!("not in a Git repository; run this inside one, or use --global")
        })?;
    }
    let program = program_path(request.program.as_deref())?;
    // Check the hook location before writing anything.
    let hook = if request.hook {
        Some(plan_hook(request.hooks_path_ok)?)
    } else {
        None
    };

    if request.verify_only {
        warn_replacing(scope, "gpg.x509.program", &program)?;
        scope.set("gpg.x509.program", &program)?;
        println!("gpg.x509.program = {program}");
        println!("Git now verifies Tartan signatures with tt; nothing else changed.");
        if let Some(hook) = hook {
            install_hook(&hook, &program)?;
        }
        return Ok(());
    }

    let did = match setup_did(ctx, request.did.as_deref())? {
        Some(did) => did,
        None => login().await?,
    };
    if request.did.is_some() && SigningSession::load(ctx, &did)?.is_none() {
        eprintln!(
            "tt: warning: no signing session is stored for {did}; run `tt sign --login --handle \
             {did}` before signing"
        );
    }
    let mut settings = vec![
        ("gpg.format", "x509".to_owned()),
        ("gpg.x509.program", program.clone()),
        ("user.signingKey", did.clone()),
        ("commit.gpgSign", "true".to_owned()),
    ];
    if !request.no_tags {
        settings.push(("tag.gpgSign", "true".to_owned()));
    }
    for (key, value) in &settings {
        if matches!(*key, "gpg.format" | "gpg.x509.program" | "user.signingKey") {
            warn_replacing(scope, key, value)?;
        }
    }
    for (key, value) in &settings {
        scope.set(key, value)?;
        println!("{key} = {value}");
    }
    if !scope.get_all("tt.trustedSigner")?.contains(&did) {
        scope.add("tt.trustedSigner", &did)?;
    }
    println!("tt.trustedSigner += {did}");
    match (&hook, request.global) {
        (Some(hook), _) => install_hook(hook, &program)?,
        (None, true) => println!(
            "The pre-push hook is per repository: run `tt sign --setup --hook` in each \
             repository where offline work is pushed, or `tt sign --flush` before pushing."
        ),
        (None, false) => println!(
            "Hint: `tt sign --setup --hook` publishes signatures queued offline before each push."
        ),
    }
    Ok(())
}

fn warn_replacing(scope: Scope, key: &str, new: &str) -> Result<()> {
    if let Some(old) = scope.get(key)?
        && old != new
    {
        eprintln!("tt: warning: replacing {key} = {old}");
    }
    Ok(())
}

/// Where the hook goes, after the checks.
struct HookPlan {
    path: PathBuf,
}

fn plan_hook(hooks_path_ok: bool) -> Result<HookPlan> {
    let path = git::hook_path()?;
    let dir = path
        .parent()
        .context("the hook path has a directory")?
        .to_path_buf();
    let common = git::common_dir()?;
    let custom = git::config_get("core.hooksPath")?.is_some();
    let outside = custom && !git::normalise(&dir).starts_with(&common);
    let tracked = git::is_tracked(&path)? || git::is_tracked(&dir)?;
    if (outside || tracked) && !hooks_path_ok {
        bail!(
            "the pre-push hook would go to {}, which is {}; tt changes nothing there unless you \
             pass --hooks-path-ok",
            path.display(),
            if tracked {
                "tracked in the working tree"
            } else {
                "a hooks directory outside this repository (core.hooksPath)"
            }
        );
    }
    let chained = dir.join("pre-push.tt-chained");
    if path.exists() && !is_marked(&path) && chained.exists() {
        bail!(
            "both {} (not tt's) and {} exist; tt will not overwrite either (nothing was changed)",
            path.display(),
            chained.display()
        );
    }
    Ok(HookPlan { path })
}

fn is_marked(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| text.lines().any(|line| line == HOOK_MARKER))
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The hook script.
pub fn hook_script(program: &str) -> String {
    format!(
        "#!/bin/sh\n\
         {HOOK_MARKER}\n\
         # Written by `tt sign --setup --hook`: publishes queued Tartan signature records, then\n\
         # runs the hook that was here before (pre-push.tt-chained) with Git's arguments and\n\
         # input. `tt sign --setup --undo` removes it and restores that hook.\n\
         tt_program={program}\n\
         if command -v \"$tt_program\" >/dev/null 2>&1; then\n\
         \t\"$tt_program\" sign --flush --hook </dev/null\n\
         else\n\
         \techo \"tt-sign-hook: $tt_program was not found; queued signature records were not published\" >&2\n\
         fi\n\
         chained=\"$(dirname \"$0\")/pre-push.tt-chained\"\n\
         if [ -x \"$chained\" ]; then\n\
         \texec \"$chained\" \"$@\"\n\
         fi\n\
         exit 0\n",
        program = shell_quote(program)
    )
}

fn install_hook(plan: &HookPlan, program: &str) -> Result<()> {
    let path = &plan.path;
    let dir = path.parent().context("the hook path has a directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    if path.exists() && !is_marked(path) {
        let chained = dir.join("pre-push.tt-chained");
        std::fs::rename(path, &chained)
            .with_context(|| format!("moving the existing hook to {}", chained.display()))?;
        println!("Kept the existing pre-push hook as {}.", chained.display());
    }
    std::fs::write(path, hook_script(program))
        .with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("making {} executable", path.display()))?;
    }
    println!("Installed the pre-push hook at {}.", path.display());
    Ok(())
}

/// `tt sign --setup --undo`.
pub fn undo(global: bool) -> Result<()> {
    let scope = if global { Scope::Global } else { Scope::Local };
    let in_force = scope.get("gpg.format")?.as_deref() == Some("x509");
    let signing_key = scope.get("user.signingKey")?;
    let tt_did = signing_key.filter(|key| key.starts_with("did:"));
    if in_force {
        scope.unset("gpg.format")?;
        println!("unset gpg.format");
    }
    if let Some(program) = scope.get("gpg.x509.program")?
        && is_tt_program(&program)
    {
        scope.unset("gpg.x509.program")?;
        println!("unset gpg.x509.program");
    }
    if let Some(did) = &tt_did {
        scope.unset("user.signingKey")?;
        println!("unset user.signingKey");
        scope.unset_value("tt.trustedSigner", did)?;
        println!("removed {did} from tt.trustedSigner");
    }
    if in_force || tt_did.is_some() {
        for key in ["commit.gpgSign", "tag.gpgSign"] {
            if scope.get(key)?.as_deref() == Some("true") {
                scope.unset(key)?;
                println!("unset {key}");
            }
        }
    }
    if !global && git::run(&["rev-parse", "--git-dir"]).is_ok() {
        let path = git::hook_path()?;
        if path.exists() && is_marked(&path) {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            println!("Removed tt's pre-push hook.");
            let chained = path.with_file_name("pre-push.tt-chained");
            if chained.exists() {
                std::fs::rename(&chained, &path)
                    .with_context(|| format!("restoring {}", path.display()))?;
                println!("Restored the original pre-push hook.");
            }
        }
    }
    Ok(())
}

fn is_tt_program(program: &str) -> bool {
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    name == "tt" || name == "tt.exe"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_carries_the_marker_and_quotes_the_program() {
        let script = hook_script("/opt/my tools/tt");
        assert!(script.lines().any(|line| line == HOOK_MARKER));
        assert!(script.contains("tt_program='/opt/my tools/tt'"));
        assert!(script.contains("sign --flush --hook </dev/null"));
        assert!(script.contains("exec \"$chained\" \"$@\""));
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn tt_programs_are_recognised() {
        assert!(is_tt_program("tt"));
        assert!(is_tt_program("/usr/local/bin/tt"));
        assert!(!is_tt_program("gitsign"));
        assert!(!is_tt_program("/usr/bin/gpgsm"));
    }
}

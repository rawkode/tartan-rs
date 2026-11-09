//! Running `git` in the environment `tt` was started with, so the settings Git itself sees
//! (its `-c` options travel in `GIT_CONFIG_PARAMETERS`, and the repository's configuration
//! applies) are the ones `tt` reads.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

/// Runs `git args…` and returns its standard output, or `None` when it exits non-zero.
pub fn output(args: &[&str]) -> Result<Option<String>> {
    let out = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("running git (is it installed?)")?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// Runs `git args…` and fails with its standard error when it exits non-zero.
pub fn run(args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("running git (is it installed?)")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `git config --get <key>` in Git's environment.
pub fn config_get(key: &str) -> Result<Option<String>> {
    Ok(output(&["config", "--get", key])?.map(|value| value.trim_end_matches('\n').to_owned()))
}

/// `git config --get-all <key>` in Git's environment.
pub fn config_get_all(key: &str) -> Result<Vec<String>> {
    Ok(output(&["config", "--get-all", key])?
        .map(|value| value.lines().map(str::to_owned).collect())
        .unwrap_or_default())
}

/// The scope a setup writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The repository (`--local`).
    Local,
    /// The user's configuration (`--global`).
    Global,
}

impl Scope {
    fn flag(self) -> &'static str {
        match self {
            Scope::Local => "--local",
            Scope::Global => "--global",
        }
    }

    /// `git config <scope> --get <key>`.
    pub fn get(self, key: &str) -> Result<Option<String>> {
        Ok(output(&["config", self.flag(), "--get", key])?
            .map(|value| value.trim_end_matches('\n').to_owned()))
    }

    /// `git config <scope> --get-all <key>`.
    pub fn get_all(self, key: &str) -> Result<Vec<String>> {
        Ok(output(&["config", self.flag(), "--get-all", key])?
            .map(|value| value.lines().map(str::to_owned).collect())
            .unwrap_or_default())
    }

    /// `git config <scope> <key> <value>`.
    pub fn set(self, key: &str, value: &str) -> Result<()> {
        run(&["config", self.flag(), key, value]).map(drop)
    }

    /// `git config <scope> --add <key> <value>`.
    pub fn add(self, key: &str, value: &str) -> Result<()> {
        run(&["config", self.flag(), "--add", key, value]).map(drop)
    }

    /// `git config <scope> --unset <key>` (no error when it is absent).
    pub fn unset(self, key: &str) -> Result<()> {
        let _ = output(&["config", self.flag(), "--unset-all", key])?;
        Ok(())
    }

    /// Removes the values of `key` equal to `value`.
    pub fn unset_value(self, key: &str, value: &str) -> Result<()> {
        let pattern = format!("^{}$", regex_escape(value));
        let _ = output(&["config", self.flag(), "--unset-all", key, &pattern])?;
        Ok(())
    }
}

fn regex_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if "\\.^$|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `git rev-parse --git-path hooks/pre-push`, absolute.
pub fn hook_path() -> Result<PathBuf> {
    let path = run(&["rev-parse", "--git-path", "hooks/pre-push"])?;
    absolute(Path::new(path.trim_end_matches('\n')))
}

/// `git rev-parse --git-common-dir`, absolute.
pub fn common_dir() -> Result<PathBuf> {
    let path = run(&["rev-parse", "--git-common-dir"])?;
    absolute(Path::new(path.trim_end_matches('\n')))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("the working directory")?
            .join(path)
    };
    Ok(normalise(&joined))
}

/// Resolves `.` and `..` components without touching the file system, and symbolic links of
/// existing ancestors through `canonicalize`.
pub fn normalise(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_owned());
                existing = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut out = existing.canonicalize().unwrap_or(existing);
    for name in rest.into_iter().rev() {
        out.push(name);
    }
    out
}

/// Whether `path` is tracked in the working tree.
pub fn is_tracked(path: &Path) -> Result<bool> {
    let Some(text) = path.to_str() else {
        return Ok(false);
    };
    Ok(output(&["ls-files", "--error-unmatch", "--", text])?.is_some())
}

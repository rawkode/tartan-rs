//! Secrets on the person's machine (design D16, *Credentials*): forge credentials, keyed by the
//! forge's origin, and signing sessions, keyed by DID.
//!
//! They go to the operating system's credential store through `keyring` (service `tt`). When no
//! store is reachable, or `TT_CREDENTIAL_STORE=file`, they go to
//! `$XDG_STATE_HOME/tt/credentials/<name>.json` (the name in base32) with mode 0600 in a 0700
//! directory, and `tt` refuses a file or directory group or others can access. Secrets never go
//! to Git configuration, `config.toml`, child processes, logs or error messages.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use crate::paths::{self, Dirs};

/// The `keyring` service name.
const SERVICE: &str = "tt";

/// Where secrets are kept.
#[derive(Debug, Clone)]
pub enum Store {
    /// The OS credential store, falling back to files in this directory when it cannot be
    /// reached.
    Keyring(PathBuf),
    /// Files in this directory only.
    File(PathBuf),
}

/// Where a secret was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written {
    /// The OS credential store.
    Keyring,
    /// A file (its path).
    File(PathBuf),
}

impl Store {
    /// The store `TT_CREDENTIAL_STORE` names (`keyring` by default, or `file`).
    pub fn from_env(dirs: &Dirs) -> Self {
        let dir = dirs.credentials();
        match std::env::var("TT_CREDENTIAL_STORE").as_deref() {
            Ok("file") => Store::File(dir),
            _ => Store::Keyring(dir),
        }
    }

    fn dir(&self) -> &PathBuf {
        match self {
            Store::Keyring(dir) | Store::File(dir) => dir,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir().join(format!("{}.json", paths::file_name(name)))
    }

    /// Reads the secret `name`, if one is stored.
    pub fn get(&self, name: &str) -> Result<Option<String>> {
        if let Store::Keyring(_) = self
            && let Some(entry) = keyring_entry(name)
        {
            match blocking(|| entry.get_password()) {
                Ok(secret) => return Ok(Some(secret)),
                Err(keyring::Error::NoEntry) => {}
                Err(_) => {}
            }
        }
        self.read_file(name)
    }

    fn read_file(&self, name: &str) -> Result<Option<String>> {
        let path = self.file(name);
        if !path.exists() {
            return Ok(None);
        }
        paths::check_private(self.dir())?;
        paths::check_private(&path)?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading the credential file {}", path.display()))?;
        Ok(Some(text))
    }

    /// Stores the secret `name`.
    pub fn set(&self, name: &str, secret: &str) -> Result<Written> {
        if let Store::Keyring(_) = self
            && let Some(entry) = keyring_entry(name)
            && blocking(|| entry.set_password(secret)).is_ok()
        {
            // A file left by an earlier fallback would shadow nothing, but would keep an old
            // secret on disk.
            let _ = std::fs::remove_file(self.file(name));
            return Ok(Written::Keyring);
        }
        let dir = self.dir();
        paths::private_dir(dir)?;
        paths::check_private(dir)?;
        let path = self.file(name);
        paths::write_private(&path, secret.as_bytes())?;
        Ok(Written::File(path))
    }

    /// Deletes the secret `name` wherever it is.
    pub fn delete(&self, name: &str) -> Result<()> {
        if let Store::Keyring(_) = self
            && let Some(entry) = keyring_entry(name)
        {
            let _ = blocking(|| entry.delete_credential());
        }
        let path = self.file(name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("deleting the credential {}", path.display()))
            }
        }
    }

    /// A one-line description of where a secret written as `written` lives.
    pub fn describe(written: &Written) -> String {
        match written {
            Written::Keyring => "in the operating system's credential store".to_owned(),
            Written::File(path) => format!(
                "in {} (mode 0600): no credential store was reachable or TT_CREDENTIAL_STORE=file",
                path.display()
            ),
        }
    }
}

fn keyring_entry(name: &str) -> Option<keyring::Entry> {
    if keyring::Entry::store_status().is_err() {
        return None;
    }
    keyring::Entry::new(SERVICE, name).ok()
}

/// Runs a credential-store call, which may block on D-Bus, outside the async executor's
/// worker threads.
fn blocking<T>(call: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(call)
        }
        _ => call(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_file_store_writes_0600_and_refuses_loosened_files() {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir().expect("temp");
        let store = Store::File(temp.path().join("credentials"));
        let written = store
            .set("https://forge.example", "tartan_cli_secret")
            .expect("set");
        let Written::File(path) = written else {
            panic!("the file store writes files");
        };
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            store.get("https://forge.example").expect("get").as_deref(),
            Some("tartan_cli_secret")
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let error = store
            .get("https://forge.example")
            .expect_err("a loosened file is refused")
            .to_string();
        assert!(
            error.contains(
                &path
                    .file_name()
                    .expect("name")
                    .to_string_lossy()
                    .to_string()
            ),
            "{error}"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        std::fs::set_permissions(
            temp.path().join("credentials"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod");
        let error = store
            .get("https://forge.example")
            .expect_err("a loosened directory is refused")
            .to_string();
        assert!(error.contains("credentials"), "{error}");
        std::fs::set_permissions(
            temp.path().join("credentials"),
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("chmod");
        store.delete("https://forge.example").expect("delete");
        assert_eq!(store.get("https://forge.example").expect("get"), None);
    }
}

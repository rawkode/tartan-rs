//! Where `tt` keeps its files (design D16): `$XDG_CONFIG_HOME/tt` for `config.toml`,
//! `$XDG_STATE_HOME/tt` for credentials, sessions, the signing queue and locks, and
//! `$XDG_CACHE_HOME/tt` for the verification cache. Each falls back to its XDG default under
//! `$HOME`.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

/// `tt`'s three directories.
#[derive(Debug, Clone)]
pub struct Dirs {
    /// `$XDG_CONFIG_HOME/tt`.
    pub config: PathBuf,
    /// `$XDG_STATE_HOME/tt`.
    pub state: PathBuf,
    /// `$XDG_CACHE_HOME/tt`.
    pub cache: PathBuf,
}

impl Dirs {
    /// The directories from the environment.
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME").filter(|value| !value.is_empty());
        let base = |name: &str, fallback: &str| -> Result<PathBuf> {
            if let Some(value) = std::env::var_os(name).filter(|value| !value.is_empty()) {
                let path = PathBuf::from(value);
                if path.is_absolute() {
                    return Ok(path);
                }
            }
            match &home {
                Some(home) => Ok(PathBuf::from(home).join(fallback)),
                None => bail!("neither {name} nor HOME is set"),
            }
        };
        Ok(Dirs {
            config: base("XDG_CONFIG_HOME", ".config")?.join("tt"),
            state: base("XDG_STATE_HOME", ".local/state")?.join("tt"),
            cache: base("XDG_CACHE_HOME", ".cache")?.join("tt"),
        })
    }

    /// The directory of the signing queue.
    pub fn queue(&self) -> PathBuf {
        self.state.join("queue")
    }

    /// The directory of refresh locks.
    pub fn locks(&self) -> PathBuf {
        self.state.join("locks")
    }

    /// The directory of credential files.
    pub fn credentials(&self) -> PathBuf {
        self.state.join("credentials")
    }
}

/// `name` in unpadded lowercase base32 (RFC 4648), for file names: DIDs and forge origins
/// hold `:`, which Windows does not allow in file names.
pub fn file_name(name: &str) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(name.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for byte in name.bytes() {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The inverse of [`file_name`], for listing queued DIDs.
pub fn from_file_name(encoded: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for c in encoded.bytes() {
        let value = match c {
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    String::from_utf8(out).ok()
}

/// Creates `dir` (and its parents) with mode 0700 where it is created.
pub fn private_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("creating {}", dir.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))
    }
}

/// Refuses `path` when group or others can access it, as ssh does with its keys.
pub fn check_private(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                bail!(
                    "{} is accessible by group or others (mode {mode:o}); tt refuses to use it. \
                     Restrict it with `chmod {} {}`",
                    path.display(),
                    if metadata.is_dir() { "700" } else { "600" },
                    path.display()
                );
            }
        }
    }
    let _ = path;
    Ok(())
}

/// Writes `contents` to `path` with mode 0600, through a temporary file in the same directory
/// and a rename, so readers never see a partial file.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().context("a file path has a directory")?;
    private_dir(dir)?;
    let temporary = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
        std::process::id()
    ));
    {
        let mut file = open_private(&temporary, true)?;
        file.write_all(contents)
            .with_context(|| format!("writing {}", temporary.display()))?;
        file.sync_all().ok();
    }
    std::fs::rename(&temporary, path).with_context(|| format!("writing {}", path.display()))
}

/// Opens `path` for writing with mode 0600 (creating it), truncating when asked, else
/// appending.
pub fn open_private(path: &Path, truncate: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).write(true);
    if truncate {
        options.truncate(true);
    } else {
        options.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

/// Opens (creating) the lock file `path` for [`File::lock`].
pub fn lock_file(path: &Path) -> Result<File> {
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("opening the lock {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_round_trip_without_colons() {
        for name in [
            "did:plc:abcdefghijklmnopqrstuvwx",
            "did:web:localhost%3A9400",
            "https://forge.example",
            "",
            "a",
        ] {
            let encoded = file_name(name);
            assert!(!encoded.contains(':') && !encoded.contains('/'));
            assert_eq!(from_file_name(&encoded).as_deref(), Some(name));
        }
        assert_eq!(file_name("f"), "my");
        assert_eq!(file_name("foobar"), "mzxw6ytboi");
    }

    #[cfg(unix)]
    #[test]
    fn private_files_have_mode_0600_in_a_0700_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir().expect("temp");
        let dir = temp.path().join("a/b");
        let file = dir.join("secret.json");
        write_private(&file, b"{}").expect("write");
        let mode = |path: &Path| {
            std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&dir), 0o700);
        check_private(&file).expect("private");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let error = check_private(&file).expect_err("loosened").to_string();
        assert!(error.contains("secret.json"), "{error}");
    }
}

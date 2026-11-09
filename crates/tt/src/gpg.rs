//! Git's signing-program interface (design D7, *`tt sign` as Git's signing program*).
//!
//! Git runs `gpg.x509.program` without a shell, as `<program> --status-fd=<n> -bsau <key>` to
//! sign and `<program> --status-fd=<n> --verify <file> -` to verify. `tt` recognises those
//! argument lists before clap and parses them by hand. A first argument of
//! `--keyid-format=long` is Git's OpenPGP verify argv, which reaches `tt` only when it was
//! configured as `gpg.program`; it is parsed here so `tt` can explain the mistake.

use std::fs::OpenOptions;
use std::io::Write as _;

/// What Git asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpgCall {
    /// Sign standard input as `key`.
    Sign {
        /// The status descriptor.
        status_fd: i32,
        /// The `-u` key: a DID, or the committer identity when `user.signingKey` is unset.
        key: String,
    },
    /// Verify the signature in `file` over standard input.
    Verify {
        /// The status descriptor.
        status_fd: i32,
        /// The signature file.
        file: String,
    },
    /// Git verifying with `tt` as its OpenPGP program (`gpg.program=tt`).
    OpenPgpVerify {
        /// The status descriptor, when given.
        status_fd: Option<i32>,
    },
}

/// Whether `args` (without the program name, and without a leading `sign`) are Git's.
pub fn is_gpg_argv(args: &[String]) -> bool {
    args.first()
        .is_some_and(|first| first.starts_with("--status-fd=") || first == "--keyid-format=long")
}

fn status_fd(arg: &str) -> Result<i32, String> {
    let value = arg
        .strip_prefix("--status-fd=")
        .ok_or_else(|| format!("expected --status-fd=<n>, got {arg:?}"))?;
    value
        .parse::<i32>()
        .ok()
        .filter(|fd| *fd >= 0)
        .ok_or_else(|| format!("--status-fd needs a descriptor number, got {value:?}"))
}

/// Parses Git's argument list. Anything but the two forms Git uses is refused.
pub fn parse(args: &[String]) -> Result<GpgCall, String> {
    let Some(first) = args.first() else {
        return Err("no arguments".to_owned());
    };
    if first == "--keyid-format=long" {
        let status_fd = args
            .iter()
            .find(|arg| arg.starts_with("--status-fd="))
            .and_then(|arg| status_fd(arg).ok());
        return Ok(GpgCall::OpenPgpVerify { status_fd });
    }
    let fd = status_fd(first)?;
    let rest = &args[1..];
    if rest.first().map(String::as_str) == Some("--verify") {
        return match rest {
            [_, file, dash] if dash == "-" && !file.starts_with('-') => Ok(GpgCall::Verify {
                status_fd: fd,
                file: file.clone(),
            }),
            _ => Err("verification is `--status-fd=<n> --verify <file> -`".to_owned()),
        };
    }
    let (mut detach, mut key) = (false, None::<String>);
    let mut index = 0;
    while index < rest.len() {
        let arg = rest[index].as_str();
        index += 1;
        let take_key = |index: &mut usize| -> Result<String, String> {
            let value = rest
                .get(*index)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "-u needs a key".to_owned())?;
            *index += 1;
            Ok(value.clone())
        };
        match arg {
            "--detach-sign" => detach = true,
            "--armor" | "--sign" => {}
            "--local-user" => key = Some(take_key(&mut index)?),
            _ if arg.starts_with("--local-user=") => {
                key = Some(arg["--local-user=".len()..].to_owned());
            }
            _ if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 1 => {
                let letters = &arg[1..];
                for (position, letter) in letters.char_indices() {
                    match letter {
                        'b' => detach = true,
                        's' | 'a' => {}
                        'u' if position == letters.len() - 1 => {
                            key = Some(take_key(&mut index)?);
                        }
                        _ => return Err(format!("unknown option {arg:?}")),
                    }
                }
            }
            _ => return Err(format!("unknown option {arg:?}")),
        }
    }
    match (detach, key) {
        (true, Some(key)) => Ok(GpgCall::Sign { status_fd: fd, key }),
        (false, _) => Err("only detached signatures (-b) are supported".to_owned()),
        (true, None) => Err("signing needs a key (-u <key>)".to_owned()),
    }
}

/// Writes `[GNUPG:]` status lines to Git's status descriptor.
pub struct StatusWriter {
    fd: i32,
}

impl StatusWriter {
    /// A writer for descriptor `fd`.
    pub fn new(fd: i32) -> Self {
        StatusWriter { fd }
    }

    /// Writes `[GNUPG:] <line>`. Every value in `line` must already be syntax-checked and free
    /// of whitespace other than the separating spaces (design D7, *Status-line hygiene*);
    /// anything with a control character is dropped rather than written.
    pub fn line(&self, line: &str) {
        if line.chars().any(char::is_control) {
            return;
        }
        let text = format!("[GNUPG:] {line}\n");
        match self.fd {
            1 => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
            }
            2 => {
                let mut out = std::io::stderr().lock();
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
            }
            fd => {
                if let Ok(mut file) = OpenOptions::new()
                    .append(true)
                    .open(format!("/dev/fd/{fd}"))
                {
                    let _ = file.write_all(text.as_bytes());
                }
            }
        }
    }
}

/// Whether `value` may appear in a status line: non-empty, at most 2048 bytes, printable ASCII
/// without whitespace.
pub fn status_safe(value: &str) -> bool {
    !value.is_empty() && value.len() <= 2048 && value.bytes().all(|b| b.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn git_sign_argv() {
        assert_eq!(
            parse(&args("--status-fd=2 -bsau did:plc:abc")),
            Ok(GpgCall::Sign {
                status_fd: 2,
                key: "did:plc:abc".into()
            })
        );
        for separated in [
            "--status-fd=2 -b -s -a -u did:plc:abc",
            "--status-fd=2 --detach-sign --armor --local-user did:plc:abc",
            "--status-fd=2 -bsa -u did:plc:abc",
            "--status-fd=2 --detach-sign --local-user=did:plc:abc",
        ] {
            assert_eq!(
                parse(&args(separated)),
                Ok(GpgCall::Sign {
                    status_fd: 2,
                    key: "did:plc:abc".into()
                }),
                "{separated}"
            );
        }
        // Git passes the committer identity when user.signingKey is unset.
        let committer = vec![
            "--status-fd=2".to_owned(),
            "-bsau".to_owned(),
            "Alice <alice@example.com>".to_owned(),
        ];
        assert_eq!(
            parse(&committer),
            Ok(GpgCall::Sign {
                status_fd: 2,
                key: "Alice <alice@example.com>".into()
            })
        );
    }

    #[test]
    fn git_verify_argv() {
        assert_eq!(
            parse(&args("--status-fd=1 --verify /tmp/sig -")),
            Ok(GpgCall::Verify {
                status_fd: 1,
                file: "/tmp/sig".into()
            })
        );
        assert_eq!(
            parse(&args(
                "--keyid-format=long --status-fd=1 --verify /tmp/sig -"
            )),
            Ok(GpgCall::OpenPgpVerify { status_fd: Some(1) })
        );
        assert!(parse(&args("--status-fd=1 --verify /tmp/sig")).is_err());
    }

    #[test]
    fn unknown_flags_are_refused() {
        assert!(parse(&args("--status-fd=2 --clearsign")).is_err());
        assert!(parse(&args("--status-fd=2 -bsau")).is_err());
        assert!(parse(&args("--status-fd=2 -bx -u k")).is_err());
        assert!(parse(&args("--status-fd=x -bsau k")).is_err());
        assert!(parse(&args("--status-fd=2 -sau k")).is_err());
        assert!(is_gpg_argv(&args("--status-fd=2 -bsau k")));
        assert!(is_gpg_argv(&args("--keyid-format=long --verify f -")));
        assert!(!is_gpg_argv(&args("sign --login")));
    }

    #[test]
    fn status_values_never_carry_whitespace() {
        assert!(status_safe("did:plc:abcdefghijklmnopqrstuvwx"));
        assert!(!status_safe("did:plc:abc\n[GNUPG:] GOODSIG x"));
        assert!(!status_safe("@alice example"));
        assert!(!status_safe(""));
    }
}

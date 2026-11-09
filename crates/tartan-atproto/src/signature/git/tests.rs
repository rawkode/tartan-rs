use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::*;
use crate::did::Did;
use crate::signature::record::{Salt, SignatureRecord, keyed_digest};
use crate::signature::tid::Tid;

fn tartan_block(salt: &Salt) -> String {
    let record = SignatureRecord::for_git(b"x", salt, "2026-11-09T12:00:00Z".into());
    SignatureBlock {
        did: Did::parse("did:plc:abcdefghijklmnopqrstuvwx", Mode::Production).expect("did"),
        rkey: Tid::parse("3lbmxyz2abc2k").expect("tid"),
        cid: record.cid(),
        salt: salt.clone(),
    }
    .write()
}

/// A block as a commit header value: continuation lines start with a space.
fn as_header(name: &str, block: &str) -> String {
    let mut lines = block.lines();
    let mut out = format!("{name} {}\n", lines.next().unwrap_or_default());
    for line in lines {
        out.push_str(&format!(" {line}\n"));
    }
    out
}

const HEAD: &str = "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a@example.com> 1700000000 +0000\ncommitter A <a@example.com> 1700000000 +0000\n";
const MESSAGE: &str = "\nhello\n";

const GITSIGN: &str = "-----BEGIN SIGNED MESSAGE-----\nMIIEOQYJKoZIhvcNAQcCoIIEKjCCBCYCAQExDTALBglghkgBZQMEAgEwCwYJKoZI\nhvcNAQcBoIIC2zCCAtcwggJdoAMCAQICFFJ1Kd5bXyDx0Yf1fAKe5H1n0F1oMAoG\n-----END SIGNED MESSAGE-----\n";
const PGP: &str =
    "-----BEGIN PGP SIGNATURE-----\n\niQEzBAABCAAdFiEE\n=abcd\n-----END PGP SIGNATURE-----\n";
const SSH: &str = "-----BEGIN SSH SIGNATURE-----\nU1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTk\n-----END SSH SIGNATURE-----\n";

#[test]
fn unsigned_commit() {
    let raw = format!("{HEAD}{MESSAGE}");
    let extracted = extract_commit(raw.as_bytes(), HashAlgorithm::Sha1).expect("one signature");
    assert_eq!(extracted.payload, raw.as_bytes());
    assert_eq!(extracted.signature, None);
}

#[test]
fn signed_commit() {
    let block = tartan_block(&Salt::from_bytes([1; 32]));
    let raw = format!("{HEAD}{}{MESSAGE}", as_header("gpgsig", &block));
    let extracted = extract_commit(raw.as_bytes(), HashAlgorithm::Sha1).expect("one signature");
    assert_eq!(extracted.payload, format!("{HEAD}{MESSAGE}").as_bytes());
    assert_eq!(extracted.signature.as_deref(), Some(block.as_bytes()));
    assert!(matches!(
        classify(&extracted.signature.unwrap_or_default(), Mode::Production),
        SignatureClass::Tartan(_)
    ));
}

#[test]
fn two_gpgsig_headers_are_refused() {
    let block = tartan_block(&Salt::from_bytes([1; 32]));
    let raw = format!(
        "{HEAD}{}{}{MESSAGE}",
        as_header("gpgsig", &block),
        as_header("gpgsig", &block)
    );
    assert_eq!(
        extract_commit(raw.as_bytes(), HashAlgorithm::Sha1),
        Err(MultipleSignatures)
    );
}

#[test]
fn both_algorithms_follow_gits_rule() {
    let sha1_block = tartan_block(&Salt::from_bytes([1; 32]));
    let sha256_block = tartan_block(&Salt::from_bytes([2; 32]));
    let raw = format!(
        "{HEAD}{}{}{MESSAGE}",
        as_header("gpgsig", &sha1_block),
        as_header("gpgsig-sha256", &sha256_block)
    );
    let unsigned = format!("{HEAD}{MESSAGE}");
    let sha1 = extract_commit(raw.as_bytes(), HashAlgorithm::Sha1).expect("sha1");
    assert_eq!(sha1.payload, unsigned.as_bytes());
    assert_eq!(sha1.signature.as_deref(), Some(sha1_block.as_bytes()));
    let sha256 = extract_commit(raw.as_bytes(), HashAlgorithm::Sha256).expect("sha256");
    assert_eq!(sha256.payload, unsigned.as_bytes());
    assert_eq!(sha256.signature.as_deref(), Some(sha256_block.as_bytes()));
    // Only the other algorithm's header: unsigned for this algorithm, header still removed.
    let only_other = format!(
        "{HEAD}{}{MESSAGE}",
        as_header("gpgsig-sha256", &sha256_block)
    );
    let extracted = extract_commit(only_other.as_bytes(), HashAlgorithm::Sha1).expect("sha1");
    assert_eq!(extracted.signature, None);
    assert_eq!(extracted.payload, unsigned.as_bytes());
}

#[test]
fn message_lines_are_never_headers() {
    let block = tartan_block(&Salt::from_bytes([1; 32]));
    let message = format!("\nsubject\n\n{}", as_header("gpgsig", &block));
    let raw = format!("{HEAD}{message}");
    let extracted = extract_commit(raw.as_bytes(), HashAlgorithm::Sha1).expect("unsigned");
    assert_eq!(extracted.signature, None);
    assert_eq!(extracted.payload, raw.as_bytes());
}

const TAG_HEAD: &str = "object 4b825dc642cb6eb9a060e54bf8d69288fbee4904\ntype commit\ntag v1.0\ntagger A <a@example.com> 1700000000 +0000\n";

#[test]
fn tag_quoting_a_marker_line() {
    let block = tartan_block(&Salt::from_bytes([3; 32]));
    let message = format!(
        "\nRelease notes quote an old signature:\n{PGP}and a Tartan one:\n-----BEGIN SIGNED MESSAGE-----\n"
    );
    let raw = format!("{TAG_HEAD}{message}{block}");
    let extracted = extract_tag(raw.as_bytes());
    assert_eq!(extracted.payload, format!("{TAG_HEAD}{message}").as_bytes());
    assert_eq!(extracted.signature.as_deref(), Some(block.as_bytes()));
    assert!(matches!(
        classify(block.as_bytes(), Mode::Production),
        SignatureClass::Tartan(_)
    ));
}

#[test]
fn tag_with_a_gpgsig_header() {
    let compat = tartan_block(&Salt::from_bytes([4; 32]));
    let own = tartan_block(&Salt::from_bytes([5; 32]));
    let header = as_header("gpgsig-sha256", &compat);
    let raw = format!("{TAG_HEAD}{header}\nmessage\n{own}");
    let extracted = extract_tag(raw.as_bytes());
    assert_eq!(
        extracted.payload,
        format!("{TAG_HEAD}\nmessage\n").as_bytes()
    );
    assert_eq!(extracted.signature.as_deref(), Some(own.as_bytes()));

    // A header and no in-body signature: unsigned, as Git sees it.
    let header_only = format!("{TAG_HEAD}{}\nmessage\n", as_header("gpgsig", &compat));
    let extracted = extract_tag(header_only.as_bytes());
    assert_eq!(extracted.signature, None);
}

#[test]
fn classification() {
    assert_eq!(
        classify(GITSIGN.as_bytes(), Mode::Production),
        SignatureClass::OtherX509
    );
    assert_eq!(
        classify(PGP.as_bytes(), Mode::Production),
        SignatureClass::Pgp
    );
    assert_eq!(
        classify(b"-----BEGIN PGP MESSAGE-----\nx\n", Mode::Production),
        SignatureClass::Pgp
    );
    assert_eq!(
        classify(SSH.as_bytes(), Mode::Production),
        SignatureClass::Ssh
    );
    assert_eq!(
        classify(b"something\n", Mode::Production),
        SignatureClass::Unknown
    );
    let bad =
        "-----BEGIN SIGNED MESSAGE-----\nTartan-Signature: 1\nRecord: at://alice.example.com/x/y\n";
    assert!(matches!(
        classify(bad.as_bytes(), Mode::Production),
        SignatureClass::BadTartan(_)
    ));
}

/// A scratch repository driven by the `git` binary, isolated from the developer's settings.
struct Repo {
    _dir: tempfile::TempDir,
    path: PathBuf,
    capture: PathBuf,
    program: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("repo");
        let capture = dir.path().join("payload");
        let program = dir.path().join("fake-x509");
        std::fs::create_dir(&path).expect("repo dir");
        let repo = Repo {
            _dir: dir,
            path,
            capture,
            program,
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo
    }

    /// Installs a fake `gpg.x509.program` that saves its standard input and prints `block`.
    fn fake_program(&self, block: &str) {
        let block_file = self.program.with_extension("block");
        std::fs::write(&block_file, block).expect("block");
        let script = format!(
            "#!/bin/sh\ncat > '{}'\ncat '{}'\nprintf '\\n[GNUPG:] BEGIN_SIGNING\\n[GNUPG:] SIG_CREATED D 0 8 00 0 x\\n' >&2\n",
            self.capture.display(),
            block_file.display()
        );
        std::fs::write(&self.program, script).expect("program");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.program, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        self.git_at(args, "1700000000 +0000")
    }

    fn git_at(&self, args: &[&str], date: &str) -> Vec<u8> {
        let output = Command::new("git")
            .current_dir(&self.path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", self.path.parent().unwrap_or(Path::new("/")))
            .env("GIT_AUTHOR_NAME", "Alice")
            .env("GIT_AUTHOR_EMAIL", "alice@example.com")
            .env("GIT_COMMITTER_NAME", "Alice")
            .env("GIT_COMMITTER_EMAIL", "alice@example.com")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .args([
                "-c",
                "gpg.format=x509",
                "-c",
                &format!("gpg.x509.program={}", self.program.display()),
                "-c",
                "user.signingKey=did:plc:abcdefghijklmnopqrstuvwx",
                "-c",
                "commit.gpgSign=false",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.path.join(name), content).expect("write");
    }

    fn captured(&self) -> Vec<u8> {
        std::fs::read(&self.capture).expect("captured payload")
    }
}

/// The signed data extracted from a commit (and a tag) is exactly what Git handed its signing
/// program.
#[test]
fn extracted_payload_equals_what_git_signed() {
    let repo = Repo::new();
    let block = tartan_block(&Salt::from_bytes([9; 32]));
    repo.fake_program(&block);
    repo.write("a.txt", "a\n");
    repo.git(&["add", "a.txt"]);
    repo.git(&["commit", "-q", "-S", "-m", "signed commit"]);
    let raw = repo.git(&["cat-file", "commit", "HEAD"]);
    let extracted = extract_commit(&raw, HashAlgorithm::Sha1).expect("one signature");
    assert_eq!(extracted.payload, repo.captured());
    assert_eq!(extracted.signature.as_deref(), Some(block.as_bytes()));

    repo.git(&[
        "tag",
        "-s",
        "-m",
        "release\n\n-----BEGIN PGP SIGNATURE-----\nquoted",
        "v1",
    ]);
    let raw = repo.git(&["cat-file", "tag", "v1"]);
    let extracted = extract_tag(&raw);
    assert_eq!(extracted.payload, repo.captured());
    assert_eq!(extracted.signature.as_deref(), Some(block.as_bytes()));
}

/// "Guessed private merge" (spec tt-sign): a conflict-free merge of two public commits has a
/// payload anyone can rebuild from the public parents, the default message and the record's
/// `createdAt`. The public record's digest still confirms nothing without the salt in the merge
/// commit's own signature.
#[test]
fn guessed_private_merge() {
    let repo = Repo::new();
    repo.fake_program(&tartan_block(&Salt::from_bytes([0; 32])));
    repo.write("base.txt", "base\n");
    repo.git(&["add", "base.txt"]);
    repo.git(&["commit", "-q", "-m", "base"]);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("feature.txt", "feature\n");
    repo.git(&["add", "feature.txt"]);
    repo.git(&["commit", "-q", "-m", "public feature"]);
    repo.git(&["checkout", "-q", "main"]);
    repo.write("main.txt", "main\n");
    repo.git(&["add", "main.txt"]);
    repo.git(&["commit", "-q", "-m", "public main"]);

    // The signer signs the merge with a fresh salt; the record goes public, the salt only into
    // the merge commit's signature.
    let salt = Salt::generate().expect("rng");
    repo.fake_program(&tartan_block(&salt));
    let created_at = "2026-11-09T12:00:00Z";
    let epoch = "1794225600 +0000";
    repo.git_at(
        &["merge", "-q", "--no-ff", "--no-edit", "-S", "feature"],
        epoch,
    );
    let payload = repo.captured();
    let record = SignatureRecord::for_git(&payload, &salt, created_at.into());

    // An observer rebuilds the payload from public facts.
    let text = |args: &[&str]| String::from_utf8_lossy(&repo.git(args)).trim().to_owned();
    let main_parent = text(&["rev-parse", "HEAD^1"]);
    let feature_parent = text(&["rev-parse", "HEAD^2"]);
    let tree = text(&["rev-parse", "HEAD^{tree}"]);
    let guess = format!(
        "tree {tree}\nparent {main_parent}\nparent {feature_parent}\nauthor Alice <alice@example.com> {epoch}\ncommitter Alice <alice@example.com> {epoch}\n\nMerge branch 'feature'\n"
    );
    assert_eq!(guess.as_bytes(), payload.as_slice(), "the guess is exact");

    let sha256: [u8; 32] = Sha256::digest(guess.as_bytes()).into();
    assert_ne!(record.digest, sha256);
    for _ in 0..64 {
        let other = Salt::generate().expect("rng");
        assert_ne!(record.digest, keyed_digest(&other, guess.as_bytes()));
    }

    // Only the salt read from the merge commit's signature matches.
    let raw = repo.git(&["cat-file", "commit", "HEAD"]);
    let extracted = extract_commit(&raw, HashAlgorithm::Sha1).expect("one signature");
    let SignatureClass::Tartan(block) =
        classify(&extracted.signature.unwrap_or_default(), Mode::Production)
    else {
        panic!("not a Tartan signature");
    };
    assert_eq!(record.check(guess.as_bytes(), &block.salt), Ok(()));
}

#[test]
fn two_signatures_of_one_payload_differ() {
    let payload = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n";
    let a = Salt::generate().expect("rng");
    let b = Salt::generate().expect("rng");
    assert_ne!(a, b);
    let first = SignatureRecord::for_git(payload, &a, "2026-11-09T12:00:00Z".into());
    let second = SignatureRecord::for_git(payload, &b, "2026-11-09T12:00:00Z".into());
    assert_ne!(first.digest, second.digest);
    assert_ne!(first.cid(), second.cid());
}

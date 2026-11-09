//! Commit signature verification after pushes and the signing rule (design D8, D11), against
//! the in-process devnet: objects are signed in-process by writing their records with the
//! devnet's record control (design D12, *Tests*), and the fake Git host serves real objects.
//!
//! Needs `RIVET_ENGINE_BINARY_PATH`. The `TestClock` stands in for hours and days, and the
//! Repo actor's `verify` action is called directly after each clock step.

mod common;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tartan_atproto::did::Did;
use tartan_atproto::signature::COLLECTION;
use tartan_atproto::signature::block::SignatureBlock;
use tartan_atproto::signature::record::{Salt, SignatureRecord};
use tartan_atproto::signature::tid::Tid;
use tartan_forge::{ReadEvents, Repo, Verify, VerifySchedules, identity_handle, repo_handle};
use tartan_identity_api::{IssueCliCode, RedeemCliCode, ReproveSignature, Reproved};

use common::{DEADLINE, Forge, PLAIN_MODULE, commit_bytes, commit_time};

const REPO: &str = "acme/api";
const CREATED_AT: &str = "2026-11-09T12:00:00.000Z";

/// A commit signed by `who`, and what its record is.
struct Signed {
    sha: String,
    payload: Vec<u8>,
    salt: Salt,
    rkey: String,
    cid: String,
}

/// Inserts a `gpgsig` header holding `signature` after the commit's headers.
fn with_gpgsig(unsigned: &[u8], signature: &str) -> Vec<u8> {
    let text = std::str::from_utf8(unsigned).expect("utf-8");
    let (headers, message) = text.split_once("\n\n").expect("headers");
    let mut lines = signature.lines();
    let mut header = format!("gpgsig {}\n", lines.next().expect("a line"));
    for line in lines {
        header.push(' ');
        header.push_str(line);
        header.push('\n');
    }
    format!("{headers}\n{header}\n{message}").into_bytes()
}

struct World {
    forge: Forge,
    alice: String,
    bob: String,
}

impl World {
    fn devnet(&self) -> &tartan_devnet::Devnet {
        &self.forge.identity.devnet
    }

    /// Signs a commit with `parents` as `who`; its record is published unless `publish` is
    /// false (then it can be published later with [`Self::publish`]).
    async fn sign(
        &self,
        who: &str,
        did: &str,
        parents: &[&str],
        message: &str,
        publish: bool,
    ) -> Signed {
        let seq = self.forge.next_seq();
        let payload = commit_bytes(parents, message, commit_time(seq));
        let salt = Salt::generate().expect("a salt");
        let record = SignatureRecord::for_git(&payload, &salt, CREATED_AT.to_owned());
        let (rkey, cid) = if publish {
            let info = self
                .devnet()
                .write_signature(who, &payload, &salt, Some(CREATED_AT.to_owned()), None)
                .await
                .expect("record written");
            (info.rkey, info.cid)
        } else {
            (Tid::now().as_str().to_owned(), record.cid().to_string())
        };
        let block = SignatureBlock {
            did: Did::parse(did, tartan_atproto::Mode::Development).expect("a DID"),
            rkey: Tid::parse(&rkey).expect("a TID"),
            cid: cid.parse().expect("a CID"),
            salt: salt.clone(),
        };
        let bytes = with_gpgsig(&payload, &block.write());
        let sha = self.forge.git.insert(REPO, "commit", bytes);
        Signed {
            sha,
            payload,
            salt,
            rkey,
            cid,
        }
    }

    /// Publishes a record that [`Self::sign`] left unpublished.
    async fn publish(&self, who: &str, signed: &Signed) {
        let info = self
            .devnet()
            .write_signature(
                who,
                &signed.payload,
                &signed.salt,
                Some(CREATED_AT.to_owned()),
                Some(&signed.rkey),
            )
            .await
            .expect("record written");
        assert_eq!(info.cid, signed.cid, "the same record");
    }

    /// An unsigned commit.
    fn unsigned(&self, parents: &[&str], message: &str) -> String {
        let seq = self.forge.next_seq();
        self.forge.git.insert(
            REPO,
            "commit",
            commit_bytes(parents, message, commit_time(seq)),
        )
    }

    /// Gives `sha` a configuration: `tartan.cue` holding `config`.
    fn configure(&self, sha: &str, config: &str) {
        self.forge.git.set_files(
            REPO,
            sha,
            BTreeMap::from([
                ("cue.mod/module.cue".to_owned(), PLAIN_MODULE.to_owned()),
                ("tartan.cue".to_owned(), config.to_owned()),
            ]),
        );
    }

    async fn repo(&self) -> rivetkit::TypedActorHandle<Repo> {
        let (status, info) = self.forge.get("/-/api/orgs/acme/repos/api").await;
        assert_eq!(status, 200, "{info}");
        let id = info["id"].as_str().expect("an id");
        repo_handle(&self.forge.client, id, None).expect("the repository actor")
    }

    async fn verify(&self) {
        self.repo()
            .await
            .send(Verify {})
            .await
            .expect("verify runs");
    }

    async fn signature(&self, sha: &str) -> (u16, Value) {
        self.forge
            .get(&format!(
                "/-/api/orgs/acme/repos/api/commits/{sha}/signature"
            ))
            .await
    }

    /// Waits until `sha`'s result is no longer pending, running `verify` meanwhile.
    async fn judged(&self, sha: &str) -> Value {
        let start = Instant::now();
        loop {
            let (status, view) = self.signature(sha).await;
            if status == 200 && view["status"] != "pending" {
                return view;
            }
            assert!(start.elapsed() < DEADLINE, "{sha}: {status} {view}");
            self.verify().await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn config_status(&self) -> Value {
        let (status, view) = self
            .forge
            .get("/-/api/orgs/acme/repos/api/config/status")
            .await;
        assert_eq!(status, 200, "{view}");
        view
    }

    /// Waits until `main`'s configuration at `sha` is handled (applied or refused).
    async fn handled(&self, sha: &str) -> Value {
        let sha = sha.to_owned();
        self.forge
            .until(
                "the configuration is handled",
                async || self.config_status().await,
                |status| status["pending"].is_null() || status["pending"]["commit"] != sha.as_str(),
            )
            .await
    }

    async fn events(&self) -> Vec<(String, Value)> {
        self.repo()
            .await
            .send(ReadEvents {
                after: 0,
                limit: 500,
            })
            .await
            .expect("events")
            .into_iter()
            .map(|envelope| (envelope.kind, envelope.data))
            .collect()
    }

    /// The `config.refused` problems recorded for `sha`.
    async fn refusals(&self, sha: &str) -> Vec<String> {
        self.events()
            .await
            .into_iter()
            .filter(|(kind, data)| kind == "config.refused" && data["sha"] == sha)
            .flat_map(|(_, data)| {
                data["problems"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|p| p.as_str().map(str::to_owned))
            })
            .collect()
    }

    async fn applied(&self) -> Option<String> {
        self.config_status().await["applied"]["commit"]
            .as_str()
            .map(str::to_owned)
    }

    fn rule(&self, signers: &[&str]) -> String {
        let signers: Vec<String> = signers.iter().map(|d| format!("{d:?}")).collect();
        format!(
            "package tartan\nsigning: require: [{{refs: [\"refs/heads/main\"], signers: [{}]}}]\n",
            signers.join(", ")
        )
    }

    async fn push(&self, ref_name: &str, old: Option<&str>, new: &str) {
        let zero = "0".repeat(40);
        self.forge
            .push_ref(REPO, ref_name, old.unwrap_or(&zero), new)
            .await;
    }
}

/// A CLI session for `did`, as `tt login` makes one.
async fn cli_session(forge: &Forge, did: &str) -> String {
    let verifier = URL_SAFE_NO_PAD.encode([7_u8; 32]);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let identity = identity_handle(&forge.client).expect("identity");
    let code = identity
        .send(IssueCliCode {
            did: did.to_owned(),
            challenge,
            device: "laptop".to_owned(),
            client_ip: "127.0.0.1".to_owned(),
        })
        .await
        .expect("answers")
        .expect("a code");
    identity
        .send(RedeemCliCode { code, verifier })
        .await
        .expect("answers")
        .expect("a session")
        .secret
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signatures_are_verified_and_the_rule_gates_configuration() {
    let forge = Forge::start("signing").await;
    forge
        .ok("/-/api/orgs", serde_json::json!({"slug": "acme"}))
        .await;
    forge
        .ok("/-/api/orgs/acme/repos", serde_json::json!({"slug": "api"}))
        .await;
    let alice = forge.identity.owner.clone();
    let bob = forge.identity.devnet.did("bob.test").await.expect("bob");
    let world = World { forge, alice, bob };
    let (alice, bob) = (world.alice.clone(), world.bob.clone());

    // An unsigned commit on a branch no rule covers is judged `unsigned`.
    let unsigned = world.unsigned(&[], "unsigned");
    world.push("refs/heads/junk", None, &unsigned).await;
    let view = world.judged(&unsigned).await;
    assert_eq!(view["status"], "unsigned", "{view}");
    let (status, refs) = world
        .forge
        .get("/-/api/orgs/acme/repos/api/signatures?ref=refs/heads/junk")
        .await;
    assert_eq!(status, 200, "{refs}");
    assert_eq!(refs["compliant"], false);
    assert_eq!(refs["covered"], false);

    // A commit signed by Alice is verified, with a proof bundle only readers can download.
    let signed = world.sign("alice.test", &alice, &[], "signed", true).await;
    world.push("refs/heads/feature", None, &signed.sha).await;
    let view = world.judged(&signed.sha).await;
    assert_eq!(view["status"], "verified", "{view}");
    assert_eq!(view["did"], alice.as_str());
    assert_eq!(
        view["record"]["uri"],
        format!("at://{alice}/{COLLECTION}/{}", signed.rkey)
    );
    let proof_url = view["proof_url"].as_str().expect("a proof").to_owned();
    let (status, bundle) = world
        .forge
        .get(proof_url.trim_start_matches(&world.forge.base))
        .await;
    assert_eq!(status, 200, "{bundle}");
    assert_eq!(bundle["format"], "tartan.signature-proof/2");
    assert_eq!(bundle["object"]["oid"], signed.sha.as_str());
    assert_eq!(
        bundle["object"]["payload_sha256"],
        hex::encode(Sha256::digest(&signed.payload))
    );
    let anonymous = reqwest::get(format!("{}{proof_url}", world.forge.base))
        .await
        .expect("answers");
    assert_eq!(anonymous.status(), 401);
    let (status, _) = world
        .forge
        .get(&format!(
            "/-/api/orgs/acme/repos/api/proofs/{}",
            "0".repeat(64)
        ))
        .await;
    assert_eq!(status, 404, "only referenced digests are served");
    // The record is now watched.
    let tracked = identity_handle(&world.forge.client)
        .expect("identity")
        .send(tartan_identity_api::SignatureRecordQuery {
            did: alice.clone(),
            rkey: signed.rkey.clone(),
        })
        .await
        .expect("answers");
    assert!(tracked.is_some(), "verification tracks the record");

    // Malformed, foreign and altered signatures.
    let ssh = with_gpgsig(
        &commit_bytes(&[], "ssh", commit_time(world.forge.next_seq())),
        "-----BEGIN SSH SIGNATURE-----\nU1NIU0lH\n-----END SSH SIGNATURE-----\n",
    );
    let ssh = world.forge.git.insert(REPO, "commit", ssh);
    world.push("refs/heads/ssh", None, &ssh).await;
    assert_eq!(world.judged(&ssh).await["status"], "unsupported_format");
    let pgp = with_gpgsig(
        &commit_bytes(&[], "pgp", commit_time(world.forge.next_seq())),
        "-----BEGIN PGP SIGNATURE-----\niQEz\n-----END PGP SIGNATURE-----\n",
    );
    let pgp = world.forge.git.insert(REPO, "commit", pgp);
    world.push("refs/heads/pgp", None, &pgp).await;
    assert_eq!(world.judged(&pgp).await["status"], "unsupported_format");
    let malformed = with_gpgsig(
        &commit_bytes(&[], "malformed", commit_time(world.forge.next_seq())),
        "-----BEGIN SIGNED MESSAGE-----\nTartan-Signature: 1\nRecord: at://alice.test/academy.rawkode.tartan.signature/3l\n-----END SIGNED MESSAGE-----\n",
    );
    let malformed = world.forge.git.insert(REPO, "commit", malformed);
    world.push("refs/heads/malformed", None, &malformed).await;
    assert_eq!(world.judged(&malformed).await["status"], "bad_signature");
    // "Altered message": the signature kept, the message changed.
    let original = world
        .sign("alice.test", &alice, &[], "original", true)
        .await;
    let raw_original = {
        let objects = world.forge.git.objects(REPO);
        let walk = objects
            .read_new_objects(&original.sha, &[], 1, &|_| false)
            .expect("a walk");
        walk.raw[0].bytes.clone()
    };
    let altered = String::from_utf8(raw_original)
        .expect("utf-8")
        .replace("original\n", "altered\n");
    let altered = world.forge.git.insert(REPO, "commit", altered.into_bytes());
    world.push("refs/heads/altered", None, &altered).await;
    let view = world.judged(&altered).await;
    assert_eq!(view["status"], "bad_signature", "{view}");
    // "Record replaced": the record at the key no longer has the signature's CID.
    let replaced = world
        .sign("alice.test", &alice, &[], "replaced", true)
        .await;
    let other = SignatureRecord::for_git(
        b"something else",
        &Salt::generate().expect("salt"),
        CREATED_AT.to_owned(),
    );
    world
        .devnet()
        .replace_record(
            "alice.test",
            COLLECTION,
            &replaced.rkey,
            other.to_json(),
            None,
        )
        .await
        .expect("replaced");
    world.push("refs/heads/replaced", None, &replaced.sha).await;
    let view = world.judged(&replaced.sha).await;
    assert_eq!(view["status"], "bad_signature", "{view}");
    assert!(
        view["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("replaced"),
        "{view}"
    );

    // A rule requiring Alice on main, set by a configuration Alice signed (no rule was in force,
    // so it is applied without waiting).
    let set_rule = world
        .sign("alice.test", &alice, &[], "set the rule", true)
        .await;
    world.configure(&set_rule.sha, &world.rule(&[&alice]));
    world.push("refs/heads/main", None, &set_rule.sha).await;
    world.handled(&set_rule.sha).await;
    let status = world.config_status().await;
    assert_eq!(
        status["applied"]["commit"],
        set_rule.sha.as_str(),
        "{status}"
    );
    assert_eq!(status["config_commit"], set_rule.sha.as_str(), "{status}");
    assert!(!status["rule"].is_null(), "{status}");
    world.judged(&set_rule.sha).await;

    // "Unsigned configuration change": refused, naming the commit; the rule stays.
    let a = world.unsigned(&[&set_rule.sha], "unsigned change");
    world.configure(&a, &world.rule(&[&alice]));
    world.push("refs/heads/main", Some(&set_rule.sha), &a).await;
    world.handled(&a).await;
    assert_eq!(
        world.refusals(&a).await,
        vec![format!("signing: {a} is unsigned")]
    );
    assert_eq!(
        world.applied().await.as_deref(),
        Some(set_rule.sha.as_str())
    );
    let violations: Vec<Value> = world
        .events()
        .await
        .into_iter()
        .filter(|(kind, _)| kind == "ref.signature.violation")
        .map(|(_, data)| data)
        .collect();
    assert_eq!(violations.last().expect("a violation")["new"], a.as_str());

    // "A then B": Alice signs B on top of the refused A; only B is judged, and it is applied.
    let b = world.sign("alice.test", &alice, &[&a], "B", true).await;
    world.configure(&b.sha, &world.rule(&[&alice]));
    world.push("refs/heads/main", Some(&a), &b.sha).await;
    world.handled(&b.sha).await;
    assert_eq!(world.applied().await.as_deref(), Some(b.sha.as_str()));

    // "Signer outside the rule": Bob's verified signature does not satisfy main's rule.
    let by_bob = world.sign("bob.test", &bob, &[&b.sha], "bob", true).await;
    world.configure(&by_bob.sha, &world.rule(&[&alice]));
    world
        .push("refs/heads/main", Some(&b.sha), &by_bob.sha)
        .await;
    world.handled(&by_bob.sha).await;
    let refused = world.refusals(&by_bob.sha).await;
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(
        refused[0].contains("may not sign refs/heads/main"),
        "{refused:?}"
    );

    // "Unsigned configuration via another branch": pushed to an unprotected branch, then main
    // fast-forwarded to it.
    let side = world.unsigned(&[&by_bob.sha], "side change");
    world.configure(&side, &world.rule(&[&alice]));
    world.push("refs/heads/side", None, &side).await;
    world.judged(&side).await;
    world
        .push("refs/heads/main", Some(&by_bob.sha), &side)
        .await;
    world.handled(&side).await;
    assert_eq!(
        world.refusals(&side).await,
        vec![format!("signing: {side} is unsigned")]
    );

    // "Signature revoked before the fast-forward": verified as Alice on an uncovered branch,
    // its record then deleted and the deletion learned, then main fast-forwarded to it.
    let revoked = world
        .sign("alice.test", &alice, &[&side], "revoked later", true)
        .await;
    world.configure(&revoked.sha, &world.rule(&[&alice]));
    world.push("refs/heads/tmp", None, &revoked.sha).await;
    assert_eq!(world.judged(&revoked.sha).await["status"], "verified");
    world
        .devnet()
        .delete_record("alice.test", COLLECTION, &revoked.rkey, None)
        .await
        .expect("deleted");
    let reproved = identity_handle(&world.forge.client)
        .expect("identity")
        .send(ReproveSignature {
            did: alice.clone(),
            rkey: revoked.rkey.clone(),
        })
        .await
        .expect("answers");
    assert!(matches!(reproved, Reproved::Revoked { .. }), "{reproved:?}");
    world
        .push("refs/heads/main", Some(&side), &revoked.sha)
        .await;
    world.handled(&revoked.sha).await;
    assert_eq!(
        world.refusals(&revoked.sha).await,
        vec![format!("signing: {} signature was revoked", revoked.sha)]
    );
    let view = world.judged(&revoked.sha).await;
    assert_eq!(
        view["status"], "verified",
        "history is not rewritten: {view}"
    );
    assert!(!view["revoked_at"].is_null(), "{view}");
    assert_eq!(view["revoked_reason"], "deleted");

    // "PDS down after a signed configuration push": pending while the PDS is down, applied once
    // it is back, with no other push.
    let down = world
        .sign("alice.test", &alice, &[&revoked.sha], "pds down", true)
        .await;
    world.configure(&down.sha, &world.rule(&[&alice]));
    world.devnet().set_switches(|s| s.pds_offline = true).await;
    world
        .push("refs/heads/main", Some(&revoked.sha), &down.sha)
        .await;
    let pending = world
        .forge
        .until(
            "the check fails once",
            async || world.signature(&down.sha).await.1,
            |view| view["status"] == "pending" && view["attempts"].as_i64().unwrap_or(0) >= 1,
        )
        .await;
    assert_eq!(pending["did"], alice.as_str(), "{pending}");
    let status = world.config_status().await;
    assert_eq!(status["pending"]["commit"], down.sha.as_str(), "{status}");
    assert_eq!(
        status["pending"]["waiting_on"]["signature_check"],
        down.sha.as_str(),
        "{status}"
    );
    let schedules = world
        .repo()
        .await
        .send(VerifySchedules {})
        .await
        .expect("schedules");
    assert!(
        schedules.len() <= 1,
        "one pending verify schedule: {schedules:?}"
    );
    world.forge.clock.advance(Duration::from_secs(10 * 60));
    world.devnet().set_switches(|s| s.pds_offline = false).await;
    world.verify().await;
    assert_eq!(world.judged(&down.sha).await["status"], "verified");
    world.handled(&down.sha).await;
    assert_eq!(world.applied().await.as_deref(), Some(down.sha.as_str()));

    // "Record published late": the record is still in the signer's queue at the push, and is
    // published twenty minutes later.
    let late = world
        .sign("alice.test", &alice, &[&down.sha], "late", false)
        .await;
    world.configure(&late.sha, &world.rule(&[&alice]));
    world
        .push("refs/heads/main", Some(&down.sha), &late.sha)
        .await;
    let pending = world
        .forge
        .until(
            "the record is missing",
            async || world.signature(&late.sha).await.1,
            |view| view["status"] == "pending" && view["attempts"].as_i64().unwrap_or(0) >= 1,
        )
        .await;
    assert!(
        pending["last_error"]
            .as_str()
            .unwrap_or_default()
            .contains("not found"),
        "{pending}"
    );
    world.forge.clock.advance(Duration::from_secs(20 * 60));
    world.publish("alice.test", &late).await;
    world.verify().await;
    assert_eq!(world.judged(&late.sha).await["status"], "verified");
    world.handled(&late.sha).await;
    assert_eq!(world.applied().await.as_deref(), Some(late.sha.as_str()));

    // "Head check gives up": the Git host fails for over an hour, so the head is refused as
    // unverifiable; an owner's re-check applies it once the host is back.
    let stuck = world
        .sign("alice.test", &alice, &[&late.sha], "stuck", true)
        .await;
    world.configure(&stuck.sha, &world.rule(&[&alice]));
    world
        .forge
        .git
        .failing
        .store(true, std::sync::atomic::Ordering::SeqCst);
    world
        .push("refs/heads/main", Some(&late.sha), &stuck.sha)
        .await;
    world
        .forge
        .until(
            "the Git host failure is seen",
            async || world.signature(&stuck.sha).await.1,
            |view| view["status"] == "pending" && view["attempts"].as_i64().unwrap_or(0) >= 1,
        )
        .await;
    for _ in 0..7 {
        world.forge.clock.advance(Duration::from_secs(10 * 60));
        world.verify().await;
    }
    let view = world.judged(&stuck.sha).await;
    assert_eq!(view["status"], "unverifiable", "{view}");
    world.handled(&stuck.sha).await;
    assert_eq!(
        world.refusals(&stuck.sha).await,
        vec![format!("signing: {} is unverifiable", stuck.sha)]
    );
    let (status, conflict) = world
        .forge
        .post(
            &format!(
                "/-/api/orgs/acme/repos/api/commits/{}/signature/recheck",
                late.sha
            ),
            Value::Null,
        )
        .await;
    assert_eq!(status, 409, "a verified result is final: {conflict}");
    // Bob may not re-check a result stored without his DID.
    let bob_session = cli_session(&world.forge, &bob).await;
    let response = reqwest::Client::new()
        .post(format!(
            "{}/-/api/orgs/acme/repos/api/commits/{}/signature/recheck",
            world.forge.base, stuck.sha
        ))
        .bearer_auth(&bob_session)
        .send()
        .await
        .expect("answers");
    assert_eq!(response.status(), 404, "someone else's commit");
    world
        .forge
        .git
        .failing
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let (status, requeued) = world
        .forge
        .post(
            &format!(
                "/-/api/orgs/acme/repos/api/commits/{}/signature/recheck",
                stuck.sha
            ),
            Value::Null,
        )
        .await;
    assert_eq!(status, 200, "{requeued}");
    assert_eq!(world.judged(&stuck.sha).await["status"], "verified");
    world.handled(&stuck.sha).await;
    assert_eq!(world.applied().await.as_deref(), Some(stuck.sha.as_str()));

    // "Rule changed in a signed descendant": Alice adds Bob to the rule on top of main.
    let change = world
        .sign("alice.test", &alice, &[&stuck.sha], "add bob", true)
        .await;
    world.configure(&change.sha, &world.rule(&[&alice, &bob]));
    world
        .push("refs/heads/main", Some(&stuck.sha), &change.sha)
        .await;
    world.handled(&change.sha).await;
    assert_eq!(
        world.applied().await.as_deref(),
        Some(change.sha.as_str()),
        "{:?}",
        world.refusals(&change.sha).await
    );
    let status = world.config_status().await;
    assert_eq!(status["config_commit"], change.sha.as_str(), "{status}");

    // "The rule changes only through descent": a signed commit from before the rule, without
    // `signing`, is refused naming the commit that set the rule, which stays in force.
    let pre_rule = world
        .sign("alice.test", &alice, &[], "before the rule", true)
        .await;
    world.configure(&pre_rule.sha, "package tartan\n");
    world
        .push("refs/heads/main", Some(&change.sha), &pre_rule.sha)
        .await;
    world.handled(&pre_rule.sha).await;
    assert_eq!(
        world.refusals(&pre_rule.sha).await,
        vec![format!(
            "signing: the signing rule can only change in a descendant of {}",
            change.sha
        )]
    );
    let status = world.config_status().await;
    assert_eq!(status["config_commit"], change.sha.as_str(), "{status}");
    assert!(!status["rule"].is_null(), "{status}");
    assert_eq!(world.applied().await.as_deref(), Some(change.sha.as_str()));

    // One pending verify schedule at most, whatever ran before.
    let schedules = world
        .repo()
        .await
        .send(VerifySchedules {})
        .await
        .expect("schedules");
    assert!(schedules.len() <= 1, "{schedules:?}");

    world.forge.stop().await;
}

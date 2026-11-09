//! The signature records the forge relies on (design D7 *Watching records*, *Confirming before
//! revoking*, D4 *Route classes*; specs commit-signatures and forge-access) and Jetstream as a
//! trigger (design D10).
//!
//! Records are written through the devnet's controls and tracked with `RecordSignature`, as
//! verification does after a push (design D8 step 6); the push side of these scenarios (a
//! revoked or withdrawn record no longer counting toward a signing rule) is tested with
//! verification itself, task 8.3.
//!
//! The deployment admits bob as a reader; carol is a stranger the forge does not know; dave and
//! erin become known as signers. The forge-wide `pre_withdrawn` cap is lowered to 6, so
//! strangers may add one only below 3.
//!
//! Needs a Rivet engine binary: set `RIVET_ENGINE_BINARY_PATH` (see `scripts/e2e.sh`).

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tartan::jetstream::{
    JetstreamOptions, JetstreamStats, options_update, socket_config, spawn as spawn_jetstream,
    subscribe_url,
};
use tartan_atproto::signature::COLLECTION;
use tartan_atproto::signature::record::Salt;
use tartan_atproto::signature::tid::Tid;
use tartan_config::deployment::Readers;
use tartan_devnet::RecordInfo;
use tartan_forge::proof_cache::{CachedProof, proof_cache};
use tartan_forge::{IdentityLimits, ReadEvents, identity_handle};
use tartan_identity_api::{
    GetJetstreamCursor, IssueCliCode, KnowDid, KnownAs, People, RecordKey, RecordSignature,
    RedeemCliCode, ReproveSignature, Reproved, RevokedReason, SignatureRecordInfo,
    SignatureRecordQuery, SignerStanding, Sweep,
};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use common::{DEADLINE, Forge, admin_token, all_scopes, browser, sign_in};

const GET_RECORD: &str = "com.atproto.sync.getRecord";
const CREATED_AT: &str = "2026-11-09T12:00:00Z";

struct World {
    forge: Forge,
    alice: String,
    bob: String,
    carol: String,
    dave: String,
    erin: String,
    /// CLI sessions by handle.
    sessions: std::sync::Mutex<std::collections::HashMap<&'static str, String>>,
}

impl World {
    fn session(&self, handle: &str) -> String {
        self.sessions.lock().expect("sessions")[handle].clone()
    }

    /// Signs everyone in again with the CLI, after the clock moved past their sessions.
    async fn renew_sessions(&self) {
        for (handle, did) in [
            ("alice.test", &self.alice),
            ("bob.test", &self.bob),
            ("carol.test", &self.carol),
            ("dave.test", &self.dave),
            ("erin.test", &self.erin),
        ] {
            let session = cli_session(&self.forge, did).await;
            self.sessions
                .lock()
                .expect("sessions")
                .insert(handle, session);
        }
    }

    fn identity(&self) -> rivetkit::TypedActorHandle<tartan_forge::Identity> {
        identity_handle(&self.forge.client).expect("identity")
    }

    /// Writes a signature record for `who`, with a fixed `createdAt` so writing it again with
    /// the same salt and key re-creates the same CID.
    async fn write(&self, who: &str, salt: &Salt, rkey: Option<&str>) -> RecordInfo {
        self.forge
            .identity
            .devnet
            .write_signature(who, b"payload", salt, Some(CREATED_AT.to_owned()), rkey)
            .await
            .expect("record written")
    }

    /// Tracks a record as verification does.
    async fn track(&self, did: &str, record: &RecordInfo) -> bool {
        self.identity()
            .send(RecordSignature {
                did: did.to_owned(),
                rkey: record.rkey.clone(),
                cid: record.cid.clone(),
                rev: record.rev.clone(),
            })
            .await
            .expect("tracked")
    }

    async fn row(&self, did: &str, rkey: &str) -> Option<SignatureRecordInfo> {
        self.identity()
            .send(SignatureRecordQuery {
                did: did.to_owned(),
                rkey: rkey.to_owned(),
            })
            .await
            .expect("row")
    }

    async fn reprove(&self, did: &str, rkey: &str) -> Reproved {
        self.identity()
            .send(ReproveSignature {
                did: did.to_owned(),
                rkey: rkey.to_owned(),
            })
            .await
            .expect("reproved")
    }

    async fn delete(&self, who: &str, rkey: &str, host: Option<usize>) {
        self.forge
            .identity
            .devnet
            .delete_record(who, COLLECTION, rkey, host)
            .await
            .expect("deleted");
    }

    async fn events(&self) -> Vec<(String, Value)> {
        self.identity()
            .send(ReadEvents {
                after: 0,
                limit: 1000,
            })
            .await
            .expect("events")
            .into_iter()
            .map(|envelope| (envelope.kind, envelope.data))
            .collect()
    }

    fn get_records(&self) -> u64 {
        self.forge.identity.devnet.xrpc_count(GET_RECORD)
    }
}

/// The refresh route, as `who` authenticates.
async fn refresh(
    base: &str,
    who: Who<'_>,
    did: &str,
    rkey: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut request = browser().post(format!(
        "{base}/-/api/people/{}/signatures/{rkey}/refresh",
        common::percent(did)
    ));
    match who {
        Who::Bearer(secret) => request = request.bearer_auth(secret),
        Who::Cookie(session) => {
            request = request
                .header("cookie", format!("tartan_session={session}"))
                .header("origin", base);
        }
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("the gateway answers");
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

#[derive(Clone, Copy)]
enum Who<'a> {
    Bearer(&'a str),
    Cookie(&'a str),
}

fn salt() -> Salt {
    Salt::generate().expect("a salt")
}

/// A fresh record key and a CID for a record the forge has never seen.
fn untracked() -> (String, String) {
    let rkey = Tid::now().as_str().to_owned();
    let cid = tartan_atproto::ipld::cid_for_dag_cbor(rkey.as_bytes()).to_string();
    (rkey, cid)
}

async fn cli_session(forge: &Forge, did: &str) -> String {
    let verifier = URL_SAFE_NO_PAD.encode([9_u8; 32]);
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

async fn eventually<F: AsyncFn() -> bool>(what: &str, check: F) {
    let start = Instant::now();
    while !check().await {
        assert!(start.elapsed() < DEADLINE, "{what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signature_records_and_jetstream() {
    let devnet_people = [
        "alice.test",
        "bob.test",
        "carol.test",
        "dave.test",
        "erin.test",
    ];
    // The forge reads `owners` and `readers` from its deployment; bob is a reader. The devnet
    // derives DIDs from its seed, so bob's is known before it starts.
    let bob = tartan_devnet::seeded_did(&tartan_devnet::DevnetConfig::default().seed, "bob.test")
        .expect("bob");
    let forge = Forge::start_with(
        "signatures",
        Readers::Dids(vec![bob.parse().expect("a DID")]),
        IdentityLimits {
            pre_withdrawn_per_did: 1_000,
            pre_withdrawn: 6,
            pre_withdrawn_for_strangers: 3,
        },
    )
    .await;
    let devnet = forge.identity.devnet.clone();
    let mut dids = Vec::new();
    for handle in devnet_people {
        dids.push(devnet.did(handle).await.expect("a seeded person"));
    }
    let mut sessions = std::collections::HashMap::new();
    for (handle, did) in devnet_people.iter().zip(&dids) {
        sessions.insert(*handle, cli_session(&forge, did).await);
    }
    let world = World {
        alice: dids[0].clone(),
        bob: dids[1].clone(),
        carol: dids[2].clone(),
        dave: dids[3].clone(),
        erin: dids[4].clone(),
        sessions: std::sync::Mutex::new(sessions),
        forge,
    };
    // The devnet is seeded the same way on every start, so bob's DID is the one admitted.
    assert_eq!(world.bob, bob);

    tracking_and_reproofs(&world).await;
    confirmation(&world).await;
    withdrawals(&world).await;
    refresh_route_rights(&world).await;
    standing_reads_local_state(&world).await;
    rate_limits(&world).await;
    jetstream(&world).await;

    world.forge.stop().await;
}

async fn tracking_and_reproofs(world: &World) {
    let base = &world.forge.base;
    let dave = &world.dave;

    // Tracking is idempotent and makes the signer known.
    let record = world.write("dave.test", &salt(), None).await;
    assert!(world.track(dave, &record).await);
    assert!(
        !world.track(dave, &record).await,
        "a second call adds nothing"
    );
    let person = world
        .identity()
        .send(People {
            dids: vec![dave.clone()],
        })
        .await
        .expect("people")
        .remove(0)
        .expect("dave");
    assert!(person.known_as.contains(&KnownAs::Signer));

    // The signer, who cannot read this forge, re-proves their own record.
    let (status, answer) = refresh(
        base,
        Who::Bearer(&world.session("dave.test")),
        dave,
        &record.rkey,
        None,
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["outcome"], "present");

    // Someone else, not an owner, is refused.
    let (status, _) = refresh(
        base,
        Who::Bearer(&world.session("carol.test")),
        dave,
        &record.rkey,
        None,
    )
    .await;
    assert_eq!(status, 403);
    // An untracked record is 404 without `withdraw`.
    let (rkey, _) = untracked();
    let (status, _) = refresh(
        base,
        Who::Bearer(&world.session("dave.test")),
        dave,
        &rkey,
        None,
    )
    .await;
    assert_eq!(status, 404);

    // A deleted record is marked revoked, with its event.
    world.delete("dave.test", &record.rkey, None).await;
    let (status, answer) = refresh(
        base,
        Who::Bearer(&world.session("dave.test")),
        dave,
        &record.rkey,
        None,
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["outcome"], "revoked");
    assert_eq!(answer["reason"], "deleted");
    let row = world.row(dave, &record.rkey).await.expect("row");
    assert_eq!(row.revoked_reason, Some(RevokedReason::Deleted));
    let events = world.events().await;
    assert!(
        events.iter().any(|(kind, data)| kind == "signature.revoked"
            && data["did"] == *dave
            && data["rkey"] == record.rkey
            && data["cid"] == record.cid
            && data["reason"] == "deleted"),
        "{events:?}"
    );

    // Revoked record re-created: the same key and CID again; a refresh, a RecordSignature
    // replay and a later proof all leave it revoked.
    let recreated_salt = salt();
    let first = world.write("dave.test", &recreated_salt, None).await;
    assert!(world.track(dave, &first).await);
    world.delete("dave.test", &first.rkey, None).await;
    assert_eq!(
        world.reprove(dave, &first.rkey).await,
        Reproved::Revoked {
            reason: RevokedReason::Deleted
        }
    );
    let again = world
        .write("dave.test", &recreated_salt, Some(&first.rkey))
        .await;
    assert_eq!(again.cid, first.cid, "the same record again");
    assert!(!world.track(dave, &again).await);
    let (_, answer) = refresh(
        base,
        Who::Bearer(&world.session("dave.test")),
        dave,
        &first.rkey,
        None,
    )
    .await;
    assert_eq!(answer["outcome"], "already_revoked");
    assert!(matches!(
        world.reprove(dave, &first.rkey).await,
        Reproved::AlreadyRevoked { .. }
    ));
    assert!(
        world
            .row(dave, &first.rkey)
            .await
            .expect("row")
            .revoked_at
            .is_some()
    );

    // A replaced record is marked revoked as `replaced`.
    let original = world.write("erin.test", &salt(), None).await;
    assert!(world.track(&world.erin, &original).await);
    world
        .forge
        .identity
        .devnet
        .replace_record("erin.test", COLLECTION, &original.rkey, json!({"$type": COLLECTION, "namespace": "git", "algorithm": "hmac-sha256", "digest": {"$bytes": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}, "createdAt": CREATED_AT}), None)
        .await
        .expect("replaced");
    assert_eq!(
        world.reprove(&world.erin, &original.rkey).await,
        Reproved::Revoked {
            reason: RevokedReason::Replaced
        }
    );
    let events = world.events().await;
    assert!(events.iter().any(|(kind, data)| kind == "signature.revoked"
        && data["rkey"] == original.rkey
        && data["reason"] == "replaced"));

    // An owner may re-prove anyone's record.
    let owned = world.write("dave.test", &salt(), None).await;
    assert!(world.track(dave, &owned).await);
    let (status, answer) = refresh(
        base,
        Who::Bearer(&world.forge.token),
        dave,
        &owned.rkey,
        None,
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["outcome"], "present");
}

async fn confirmation(world: &World) {
    let devnet = &world.forge.identity.devnet;
    let dave = &world.dave;

    // A refresh while the PDS is down changes nothing.
    let record = world.write("dave.test", &salt(), None).await;
    assert!(world.track(dave, &record).await);
    world.delete("dave.test", &record.rkey, None).await;
    devnet.set_switches(|s| s.pds_offline = true).await;
    assert!(matches!(
        world.reprove(dave, &record.rkey).await,
        Reproved::Transient { .. }
    ));
    devnet.set_switches(|s| s.pds_offline = false).await;
    assert_eq!(
        world.row(dave, &record.rkey).await.expect("row").revoked_at,
        None
    );

    // Fresh resolution fails: the absence is proved from stored evidence, but re-resolving
    // the DID cannot reach the PLC directory, so nothing is revoked.
    devnet.set_switches(|s| s.plc_offline = true).await;
    let outcome = world.reprove(dave, &record.rkey).await;
    devnet.set_switches(|s| s.plc_offline = false).await;
    assert!(matches!(outcome, Reproved::Transient { .. }), "{outcome:?}");
    assert_eq!(
        world.row(dave, &record.rkey).await.expect("row").revoked_at,
        None
    );
    // With the directory back, the same absence revokes it.
    assert_eq!(
        world.reprove(dave, &record.rkey).await,
        Reproved::Revoked {
            reason: RevokedReason::Deleted
        }
    );

    // Older repository state: an absence proved at a revision older than the tracked one is
    // stale, so nothing is revoked.
    let record = world.write("dave.test", &salt(), None).await;
    let future = Tid::from_parts(
        (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970")
            .as_micros()
            + 3_600_000_000) as u64,
        0,
    );
    assert!(
        world
            .identity()
            .send(RecordSignature {
                did: dave.clone(),
                rkey: record.rkey.clone(),
                cid: record.cid.clone(),
                rev: future.as_str().to_owned(),
            })
            .await
            .expect("tracked")
    );
    world.delete("dave.test", &record.rkey, None).await;
    let outcome = world.reprove(dave, &record.rkey).await;
    assert!(
        matches!(&outcome, Reproved::Transient { reason } if reason.contains("older")),
        "{outcome:?}"
    );
    assert_eq!(
        world.row(dave, &record.rkey).await.expect("row").revoked_at,
        None
    );
}

async fn withdrawals(world: &World) {
    let base = &world.forge.base;
    let (dave, bob, carol, alice) = (&world.dave, &world.bob, &world.carol, &world.alice);
    let withdraw = |cid: &str| Some(json!({"withdraw": true, "cid": cid}));

    // Signer without rights withdraws a tracked record with a CLI session; the CID sent need
    // not match the tracked one, and the event records the CID sent.
    let record_salt = salt();
    let record = world.write("dave.test", &record_salt, None).await;
    assert!(world.track(dave, &record).await);
    let (_, other_cid) = untracked();
    let (status, answer) = refresh(
        base,
        Who::Bearer(&world.session("dave.test")),
        dave,
        &record.rkey,
        withdraw(&other_cid),
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["outcome"], "withdrawn");
    let row = world.row(dave, &record.rkey).await.expect("row");
    assert_eq!(row.revoked_reason, Some(RevokedReason::Withdrawn));
    assert_eq!(row.cid, record.cid, "the tracked CID is kept");
    let events = world.events().await;
    assert!(events.iter().any(|(kind, data)| kind == "signature.revoked"
        && data["rkey"] == record.rkey
        && data["cid"] == other_cid
        && data["reason"] == "withdrawn"));

    // Withdrawal wins the race: the record is re-created with the same CID before any
    // re-proof, and stays revoked.
    world.delete("dave.test", &record.rkey, None).await;
    let recreated = world
        .write("dave.test", &record_salt, Some(&record.rkey))
        .await;
    assert_eq!(recreated.cid, record.cid);
    assert!(matches!(
        world.reprove(dave, &record.rkey).await,
        Reproved::AlreadyRevoked {
            reason: Some(RevokedReason::Withdrawn)
        }
    ));

    // Owner cannot withdraw someone else's record.
    let owned = world.write("dave.test", &salt(), None).await;
    assert!(world.track(dave, &owned).await);
    let (status, _) = refresh(
        base,
        Who::Bearer(&world.session("alice.test")),
        dave,
        &owned.rkey,
        withdraw(&owned.cid),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(
        world.row(dave, &owned.rkey).await.expect("row").revoked_at,
        None
    );

    // Access token cannot withdraw, even with every scope and for its own DID.
    let mine = world.write("alice.test", &salt(), None).await;
    assert!(world.track(alice, &mine).await);
    let token = admin_token(&world.forge.client, alice, all_scopes()).await;
    let (status, _) = refresh(
        base,
        Who::Bearer(&token),
        alice,
        &mine.rkey,
        withdraw(&mine.cid),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(
        world.row(alice, &mine.rkey).await.expect("row").revoked_at,
        None
    );
    // A body-less re-proof is allowed to a token.
    let (status, answer) = refresh(base, Who::Bearer(&token), alice, &mine.rkey, None).await;
    assert_eq!(status, 200, "{answer}");

    // Reader signer withdraws, from a browser session.
    let bob_browser = sign_in(base, "bob.test", "/", None).await.session();
    let read = world.write("bob.test", &salt(), None).await;
    assert!(world.track(bob, &read).await);
    let (status, answer) = refresh(
        base,
        Who::Cookie(&bob_browser),
        bob,
        &read.rkey,
        withdraw(&read.cid),
    )
    .await;
    assert_eq!(status, 200, "{answer}");

    // Withdrawn before the forge saw it: an untracked record gets an already-revoked row that
    // tracking leaves alone. (1 of the lowered cap of 6.)
    let early = world.write("dave.test", &salt(), None).await;
    let (status, answer) = refresh(
        base,
        Who::Bearer(&world.session("dave.test")),
        dave,
        &early.rkey,
        withdraw(&early.cid),
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["outcome"], "pre_withdrawn");
    assert!(!world.track(dave, &early).await);
    let row = world.row(dave, &early.rkey).await.expect("row");
    assert!(row.pre_withdrawn && row.revoked_at.is_some());

    // Pre-withdrawal does not expire: re-created with the same CID, 31 days and several
    // sweeps later the row is still there and still revoked. (2 of 6.)
    let kept_salt = salt();
    let kept = world.write("dave.test", &kept_salt, None).await;
    let (status, _) = refresh(
        base,
        Who::Bearer(&world.session("dave.test")),
        dave,
        &kept.rkey,
        withdraw(&kept.cid),
    )
    .await;
    assert_eq!(status, 200);
    world.delete("dave.test", &kept.rkey, None).await;
    let back = world.write("dave.test", &kept_salt, Some(&kept.rkey)).await;
    assert_eq!(back.cid, kept.cid);
    world
        .forge
        .clock
        .advance(Duration::from_secs(31 * 24 * 3600));
    for _ in 0..3 {
        world.identity().send(Sweep {}).await.expect("sweep");
    }
    let row = world.row(dave, &kept.rkey).await.expect("the row is kept");
    assert!(row.revoked_at.is_some() && row.pre_withdrawn);
    let standing = world
        .identity()
        .send(SignerStanding {
            pairs: vec![RecordKey {
                did: dave.clone(),
                rkey: kept.rkey.clone(),
            }],
        })
        .await
        .expect("standing");
    assert!(standing[0].revoked_at.is_some());
    assert!(
        !world.track(dave, &back).await,
        "a later verification leaves it revoked"
    );
    // Sessions expired while the clock moved; everyone signs in again.
    world.renew_sessions().await;
    let bob_browser = sign_in(base, "bob.test", "/", None).await.session();

    // Strangers may withdraw in advance only below half the cap: carol, unknown to the forge,
    // adds the third row and is refused the fourth. (3 of 6.)
    let (rkey, cid) = untracked();
    let (status, answer) = refresh(
        base,
        Who::Bearer(&world.session("carol.test")),
        carol,
        &rkey,
        withdraw(&cid),
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    let (rkey, cid) = untracked();
    let (status, _) = refresh(
        base,
        Who::Bearer(&world.session("carol.test")),
        carol,
        &rkey,
        withdraw(&cid),
    )
    .await;
    assert_eq!(status, 429);
    assert!(world.row(carol, &rkey).await.is_none());

    // A reader is still accepted, up to the forge-wide cap. (4, 5 and 6 of 6.)
    for _ in 0..3 {
        let (rkey, cid) = untracked();
        let (status, answer) =
            refresh(base, Who::Cookie(&bob_browser), bob, &rkey, withdraw(&cid)).await;
        assert_eq!(status, 200, "{answer}");
    }
    // Forge-wide cap: another untracked withdrawal answers 429 and stores nothing…
    let (rkey, cid) = untracked();
    let (status, answer) =
        refresh(base, Who::Cookie(&bob_browser), bob, &rkey, withdraw(&cid)).await;
    assert_eq!(status, 429, "{answer}");
    assert!(world.row(bob, &rkey).await.is_none());
    // …while a withdrawal of a tracked record by another DID succeeds.
    let (status, answer) = refresh(
        base,
        Who::Bearer(&world.session("dave.test")),
        dave,
        &owned.rkey,
        withdraw(&owned.cid),
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    assert!(
        world
            .row(dave, &owned.rkey)
            .await
            .expect("row")
            .revoked_at
            .is_some()
    );
}

async fn refresh_route_rights(world: &World) {
    // The refresh route's `self` class does not depend on the read mode: a person with no
    // rights and a reader may call it for their own DID under every mode.
    let carol_record = world.write("carol.test", &salt(), None).await;
    let bob_record = world.write("bob.test", &salt(), None).await;
    // Tracking makes carol a known signer; strangers' limits were tested above.
    assert!(world.track(&world.carol, &carol_record).await);
    assert!(world.track(&world.bob, &bob_record).await);
    let owners = world.forge.gateway(Readers::Owners, Vec::new()).await;
    let signed_in = world.forge.gateway(Readers::SignedIn, Vec::new()).await;
    let listed = world
        .forge
        .gateway(
            Readers::Dids(vec![world.bob.parse().expect("a DID")]),
            Vec::new(),
        )
        .await;
    for base in [&owners, &signed_in, &listed] {
        for (handle, did, record) in [
            ("carol.test", &world.carol, &carol_record),
            ("bob.test", &world.bob, &bob_record),
        ] {
            let (status, answer) = refresh(
                base,
                Who::Bearer(&world.session(handle)),
                did,
                &record.rkey,
                None,
            )
            .await;
            assert_eq!(status, 200, "{handle} at {base}: {answer}");
            assert_eq!(answer["outcome"], "present");
        }
    }
    // Under the default `readers: "owners"`, a signer who cannot read withdraws with a CLI
    // session.
    let (status, answer) = refresh(
        &owners,
        Who::Bearer(&world.session("carol.test")),
        &world.carol,
        &carol_record.rkey,
        Some(json!({"withdraw": true, "cid": carol_record.cid})),
    )
    .await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["outcome"], "withdrawn");
}

async fn standing_reads_local_state(world: &World) {
    let devnet = &world.forge.identity.devnet;
    let (rkey, _) = untracked();
    let before = world.get_records();
    devnet
        .set_switches(|s| {
            s.pds_offline = true;
            s.plc_offline = true;
        })
        .await;
    let pairs = vec![
        RecordKey {
            did: world.dave.clone(),
            rkey: rkey.clone(),
        },
        RecordKey {
            did: "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            rkey,
        },
    ];
    let standing = world
        .identity()
        .send(SignerStanding { pairs })
        .await
        .expect("standing");
    devnet
        .set_switches(|s| {
            s.pds_offline = false;
            s.plc_offline = false;
        })
        .await;
    assert_eq!(standing.len(), 2);
    assert!(
        standing
            .iter()
            .all(|s| s.revoked_at.is_none() && !s.inactive)
    );
    assert_eq!(world.get_records(), before, "nothing was fetched");
}

async fn rate_limits(world: &World) {
    // Re-proofs are rate-limited per signer: once refused, no request reaches the PDS.
    let record = world.write("erin.test", &salt(), None).await;
    assert!(world.track(&world.erin, &record).await);
    let mut refused = None;
    for _ in 0..=101 {
        let before = world.get_records();
        let (status, _) = refresh(
            &world.forge.base,
            Who::Bearer(&world.session("erin.test")),
            &world.erin,
            &record.rkey,
            None,
        )
        .await;
        if status == 429 {
            refused = Some(before);
            break;
        }
        assert_eq!(status, 200);
    }
    let before = refused.expect("re-proofs are rate-limited");
    let (status, _) = refresh(
        &world.forge.base,
        Who::Bearer(&world.session("erin.test")),
        &world.erin,
        &record.rkey,
        None,
    )
    .await;
    assert_eq!(status, 429);
    assert_eq!(world.get_records(), before, "refused calls fetch nothing");
}

// Jetstream ---------------------------------------------------------------------------------

fn count(counter: &AtomicU64) -> u64 {
    JetstreamStats::get(counter)
}

/// Emits an identity event for the owner and waits until the task has handled it, so every
/// event before it has been handled too.
async fn settle(world: &World, stats: &JetstreamStats) {
    let before = count(&stats.refreshes);
    world
        .forge
        .identity
        .devnet
        .emit_jetstream(&world.alice, "identity", json!({}))
        .await
        .expect("emitted");
    eventually("the task handles the sentinel", async || {
        count(&stats.refreshes) > before
    })
    .await;
}

fn options(world: &World) -> JetstreamOptions {
    let mut options = JetstreamOptions::new(&world.forge.identity.devnet.jetstream_url());
    options.watch_every = Duration::from_millis(200);
    options.cursor_every = Duration::from_millis(200);
    options.reconnect_after = Duration::from_millis(200);
    options
}

async fn jetstream(world: &World) {
    let devnet = world.forge.identity.devnet.clone();
    wanted_dids_at_the_limit(world).await;

    let cancel = CancellationToken::new();
    let stats: Arc<JetstreamStats> =
        spawn_jetstream(world.forge.client.clone(), options(world), cancel.clone());
    eventually("the task connects and sends its filter", async || {
        !stats.filters().is_empty()
    })
    .await;
    settle(world, &stats).await;

    // Deleted within the reuse window: a proof cached by a verification less than ten minutes
    // earlier does not hide the deletion, which is proved from a fresh fetch.
    let bob = &world.bob;
    let record = world.write("bob.test", &salt(), None).await;
    assert!(world.track(bob, &record).await);
    proof_cache().put(
        bob,
        &record.rkey,
        CachedProof {
            pds: devnet.url().to_owned(),
            atproto_key: "did:key:stale".to_owned(),
            car: Arc::new(Vec::new()),
            fetched_at_ms: tartan_forge::Clock::now_ms(world.forge.clock.as_ref()),
        },
    );
    world.forge.clock.advance(Duration::from_secs(5 * 60));
    world.delete("bob.test", &record.rkey, None).await;
    eventually("the Jetstream delete revokes the record", async || {
        world
            .row(bob, &record.rkey)
            .await
            .is_some_and(|row| row.revoked_reason == Some(RevokedReason::Deleted))
    })
    .await;
    assert!(
        proof_cache()
            .get(
                bob,
                &record.rkey,
                tartan_forge::Clock::now_ms(world.forge.clock.as_ref())
            )
            .is_none(),
        "the record's cached proof was dropped"
    );

    // Forged deletion: Jetstream reports a delete the PDS does not prove; nothing is revoked.
    let present = world.write("bob.test", &salt(), None).await;
    assert!(world.track(bob, &present).await);
    settle(world, &stats).await;
    let reproves = count(&stats.reproves);
    devnet
        .emit_jetstream(
            bob,
            "commit",
            json!({"operation": "delete", "collection": COLLECTION, "rkey": present.rkey}),
        )
        .await
        .expect("emitted");
    settle(world, &stats).await;
    assert_eq!(count(&stats.reproves), reproves + 1);
    assert_eq!(
        world.row(bob, &present.rkey).await.expect("row").revoked_at,
        None
    );

    // New records ignored: creates by a known DID fetch nothing.
    let fetched = world.get_records();
    let creates = count(&stats.creates_ignored);
    for _ in 0..3 {
        world.write("bob.test", &salt(), None).await;
    }
    settle(world, &stats).await;
    assert_eq!(count(&stats.creates_ignored), creates + 3);
    assert_eq!(world.get_records(), fetched, "creates cause no fetch");

    // A post by a known DID triggers nothing; the server's collection filter keeps it away.
    let reproves = count(&stats.reproves);
    devnet
        .write_record(
            "bob.test",
            "app.bsky.feed.post",
            None,
            json!({"$type": "app.bsky.feed.post", "text": "hi", "createdAt": CREATED_AT}),
            None,
        )
        .await
        .expect("posted");
    settle(world, &stats).await;
    assert_eq!(count(&stats.other_collections), 0);
    assert_eq!(count(&stats.reproves), reproves);

    // After a later DID-set update the collection filter is still in force.
    let updates = stats.filters().len();
    world
        .identity()
        .send(KnowDid {
            // A DID a signing rule names, new to the forge.
            did: "did:plc:yyyyyyyyyyyyyyyyyyyyyyyy".to_owned(),
            because: KnownAs::RuleSigner,
        })
        .await
        .expect("known");
    eventually("a new filter is sent", async || {
        stats.filters().len() > updates
    })
    .await;
    devnet
        .write_record(
            "carol.test",
            "app.bsky.feed.post",
            None,
            json!({"$type": "app.bsky.feed.post", "text": "hi", "createdAt": CREATED_AT}),
            None,
        )
        .await
        .expect("posted");
    settle(world, &stats).await;
    assert_eq!(count(&stats.other_collections), 0);
    assert!(
        stats
            .filters()
            .iter()
            .all(|filter| filter.collections == [COLLECTION] && filter.dids > 0),
        "{:?}",
        stats.filters()
    );

    // Stale PDS after migration: erin moves to the second host with a new key, Jetstream is
    // not told, and the record is then deleted from the old host's copy. The forge re-resolves
    // erin, finds the record at the new PDS with its original CID, and revokes nothing.
    let erin = &world.erin;
    let moved = world.write("erin.test", &salt(), None).await;
    assert!(world.track(erin, &moved).await);
    let before = world
        .identity()
        .send(People {
            dids: vec![erin.clone()],
        })
        .await
        .expect("people")
        .remove(0)
        .expect("erin");
    devnet.migrate("erin.test", true).await.expect("migrated");
    let reproves = count(&stats.reproves);
    world.delete("erin.test", &moved.rkey, Some(0)).await;
    settle(world, &stats).await;
    assert_eq!(
        count(&stats.reproves),
        reproves + 1,
        "the delete was handled"
    );
    assert_eq!(
        world.row(erin, &moved.rkey).await.expect("row").revoked_at,
        None
    );
    let after = world
        .identity()
        .send(People {
            dids: vec![erin.clone()],
        })
        .await
        .expect("people")
        .remove(0)
        .expect("erin");
    assert_eq!(
        after.pds.as_deref(),
        Some(devnet.migration_url().await.as_str())
    );
    assert_ne!(after.atproto_key, before.atproto_key);

    // The cursor is stored.
    eventually("the cursor is stored", async || {
        world
            .identity()
            .send(GetJetstreamCursor {})
            .await
            .expect("cursor")
            .is_some()
    })
    .await;

    // Above the wantedDids limit the task sends an empty list and filters itself: identity
    // and account events for DIDs the forge does not watch are ignored.
    let mut unfiltered = options(world);
    unfiltered.max_wanted_dids = 1;
    let stopping = CancellationToken::new();
    let fallback = spawn_jetstream(world.forge.client.clone(), unfiltered, stopping.clone());
    eventually("the fallback task sends its filter", async || {
        !fallback.filters().is_empty()
    })
    .await;
    assert_eq!(fallback.filters()[0].dids, 0);
    assert_eq!(fallback.filters()[0].collections, [COLLECTION]);
    // The task resumed from the stored cursor, rewound by 5 seconds, so it first replays
    // recent events; wait until it is quiet before counting.
    let mut seen = count(&fallback.events);
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = count(&fallback.events);
        if now == seen {
            break;
        }
        seen = now;
    }
    let ignored = count(&fallback.ignored_unwatched);
    let stranger = "did:plc:zzzzzzzzzzzzzzzzzzzzzzzz";
    devnet
        .emit_jetstream(stranger, "identity", json!({}))
        .await
        .expect("emitted");
    devnet
        .emit_jetstream(
            stranger,
            "account",
            json!({"active": false, "status": "takendown"}),
        )
        .await
        .expect("emitted");
    eventually("both events for the stranger are ignored", async || {
        count(&fallback.ignored_unwatched) >= ignored + 2
    })
    .await;
    let row = world
        .identity()
        .send(People {
            dids: vec![stranger.to_owned()],
        })
        .await
        .expect("people")
        .remove(0);
    assert!(row.is_none(), "nothing was refreshed for the stranger");
    stopping.cancel();
    cancel.cancel();
}

/// Jetstream accepts an `options_update` carrying 10,000 DIDs, and the connection keeps
/// delivering events for them.
async fn wanted_dids_at_the_limit(world: &World) {
    let devnet = &world.forge.identity.devnet;
    let mut dids: Vec<String> = (0..9_999_u32)
        .map(|n| {
            let mut id = String::new();
            let mut rest = n;
            for _ in 0..24 {
                id.push(char::from(b'a' + u8::try_from(rest % 26).expect("small")));
                rest /= 26;
            }
            format!("did:plc:{id}")
        })
        .collect();
    dids.push(world.alice.clone());
    assert_eq!(dids.len(), 10_000);
    let url = subscribe_url(&devnet.jetstream_url(), None).expect("url");
    let (socket, _) =
        tokio_tungstenite::connect_async_with_config(url.as_str(), Some(socket_config()), false)
            .await
            .expect("connected");
    let (mut sink, mut stream) = socket.split();
    let update = options_update(&dids);
    assert!(update.len() > 300_000, "about 320 KB");
    sink.send(Message::text(update)).await.expect("sent");
    // Give the server a moment to apply the filter before the event.
    tokio::time::sleep(Duration::from_millis(300)).await;
    devnet
        .emit_jetstream(&world.alice, "identity", json!({}))
        .await
        .expect("emitted");
    let received = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(message) = stream.next().await {
            if let Ok(Message::Text(text)) = message {
                let event: Value = serde_json::from_str(text.as_str()).expect("json");
                if event["did"] == world.alice.as_str() {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(
        received,
        "the connection stayed open and delivered the event"
    );
}

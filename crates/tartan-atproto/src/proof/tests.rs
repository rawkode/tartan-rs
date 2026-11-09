use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jacquard_common::types::tid::Tid as JTid;
use jacquard_repo::BlockStore;
use jacquard_repo::commit::SigningKey;

use super::*;
use crate::Mode;
use crate::fetch::FetchConfig;
use crate::keys::Curve;
use crate::signature::record::Salt;
use crate::testutil::{Reply, TestServer};

const DID: &str = "did:plc:abcdefghijklmnopqrstuvwx";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

fn rev_at(ms: u64) -> JTid {
    JTid::new(crate::signature::tid::Tid::from_parts(ms * 1000, 1).as_str()).expect("tid")
}

/// A repository built with jacquard-repo: records, their MST and a signed commit.
struct TestRepo {
    store: Arc<jacquard_repo::MemoryBlockStore>,
    mst: Mst<jacquard_repo::MemoryBlockStore>,
    commit: Commit<SmolStr>,
}

impl TestRepo {
    async fn build(
        did: &str,
        key: &impl SigningKey,
        records: &[(String, Vec<u8>)],
        rev: JTid,
    ) -> Self {
        let store = Arc::new(jacquard_repo::MemoryBlockStore::new());
        let mut mst = Mst::new(store.clone());
        for (path, block) in records {
            let cid = store.put(block).await.expect("put");
            mst = mst.add(path, cid).await.expect("add");
        }
        let root = mst.persist().await.expect("persist");
        let commit = Commit::new_unsigned(
            JDid::<SmolStr>::new_owned(did).expect("did"),
            root,
            rev,
            None,
        )
        .sign(key)
        .expect("sign");
        TestRepo { store, mst, commit }
    }

    /// The `getRecord` CAR for `path`: the commit, the MST path and the record block if any.
    async fn car(&self, path: &str) -> Vec<u8> {
        self.car_with(path, |_| {}).await
    }

    async fn path_blocks(&self, path: &str) -> Vec<Cid> {
        self.mst.cids_for_path(path).await.expect("path")
    }

    async fn car_with(&self, path: &str, edit: impl FnOnce(&mut BTreeMap<Cid, Bytes>)) -> Vec<u8> {
        let commit_cid = self.commit.to_cid().expect("cid");
        let mut blocks = BTreeMap::new();
        blocks.insert(
            commit_cid,
            Bytes::from(self.commit.to_cbor().expect("cbor")),
        );
        for cid in self.path_blocks(path).await {
            if let Some(block) = self.store.get(&cid).await.expect("get") {
                blocks.insert(cid, block);
            }
        }
        edit(&mut blocks);
        jacquard_repo::car::write_car_bytes(commit_cid, blocks)
            .await
            .expect("car")
    }
}

fn k256_key() -> (k256::ecdsa::SigningKey, AtprotoKey) {
    let signing = k256::ecdsa::SigningKey::from_slice(&[11u8; 32]).expect("key");
    let public = AtprotoKey::from_sec1(
        Curve::K256,
        signing.verifying_key().to_encoded_point(true).as_bytes(),
    )
    .expect("key");
    (signing, public)
}

fn record_ref(did: &str, path: &str) -> RecordRef {
    let (collection, rkey) = path.split_once('/').expect("path");
    RecordRef {
        did: Did::parse(did, Mode::Production).expect("did"),
        collection: collection.to_owned(),
        rkey: rkey.to_owned(),
    }
}

fn signature_path(rkey: &str) -> String {
    format!("{COLLECTION}/{rkey}")
}

/// Many records, so the MST has several levels and paths of more than one node.
fn records(count: usize) -> Vec<(String, Vec<u8>)> {
    (0..count)
        .map(|i| {
            let rkey = crate::signature::tid::Tid::from_parts(1_700_000_000_000_000 + i as u64, 7);
            let block = serde_ipld_dagcbor::to_vec(&serde_json::json!({"n": i})).expect("cbor");
            (signature_path(rkey.as_str()), block)
        })
        .collect()
}

#[tokio::test]
async fn present_record() {
    let (signing, key) = k256_key();
    let records = records(50);
    let repo = TestRepo::build(DID, &signing, &records, rev_at(now_ms())).await;
    let (path, block) = &records[17];
    let car = repo.car(path).await;
    match prove_record(car, record_ref(DID, path), key, now_ms()).await {
        ProofOutcome::Present {
            record_cid,
            record,
            commit,
        } => {
            assert_eq!(&record, block);
            assert_eq!(record_cid, crate::ipld::cid_for_dag_cbor(block));
            assert_eq!(commit.rev, repo.commit.rev.as_str());
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn substituted_record_block() {
    let (signing, key) = k256_key();
    let records = records(10);
    let repo = TestRepo::build(DID, &signing, &records, rev_at(now_ms())).await;
    let (path, block) = &records[3];
    let original = crate::ipld::cid_for_dag_cbor(block);
    let car = repo
        .car_with(path, |blocks| {
            let forged =
                serde_ipld_dagcbor::to_vec(&serde_json::json!({"n": "forged"})).expect("cbor");
            blocks.insert(original, Bytes::from(forged));
        })
        .await;
    assert!(matches!(
        prove_record(car, record_ref(DID, path), key, now_ms()).await,
        ProofOutcome::Invalid(ProofFailure::Car(_))
    ));
}

#[tokio::test]
async fn high_s_p256_commit_signature() {
    let signing = p256::ecdsa::SigningKey::from_slice(&[12u8; 32]).expect("key");
    let key = AtprotoKey::from_sec1(
        Curve::P256,
        signing.verifying_key().to_encoded_point(true).as_bytes(),
    )
    .expect("key");
    let records = records(5);
    let mut repo = TestRepo::build(DID, &signing, &records, rev_at(now_ms())).await;
    let signature = p256::ecdsa::Signature::from_slice(&repo.commit.sig).expect("signature");
    let low = signature.normalize_s().unwrap_or(signature);
    let (r, s) = low.split_scalars();
    let high = p256::ecdsa::Signature::from_scalars(r, -*s).expect("high-S");
    repo.commit.sig = Bytes::from(high.to_bytes().to_vec());
    let (path, _) = &records[2];
    let car = repo.car(path).await;

    // jacquard-repo alone accepts the malleable signature...
    let did = JDid::<SmolStr>::new_owned(DID).expect("did");
    let claim = RecordClaim {
        collection: CowStr::Owned(SmolStr::from(COLLECTION)),
        rkey: CowStr::Owned(SmolStr::from(path.split_once('/').expect("path").1)),
        cid: Some(crate::ipld::cid_for_dag_cbor(&records[2].1)),
    };
    let output = verify_proofs(&car, vec![claim.clone()], &did, &key.to_jacquard())
        .await
        .expect("jacquard accepts high-S");
    assert!(output.verified.contains(&claim));

    // ...and prove_record refuses it.
    assert!(matches!(
        prove_record(car, record_ref(DID, path), key, now_ms()).await,
        ProofOutcome::Invalid(ProofFailure::Commit(_))
    ));
}

#[tokio::test]
async fn wrong_did_and_wrong_key() {
    let (signing, key) = k256_key();
    let records = records(5);
    let repo = TestRepo::build(DID, &signing, &records, rev_at(now_ms())).await;
    let (path, _) = &records[1];
    let car = repo.car(path).await;
    let failure = prove_record(
        car.clone(),
        record_ref("did:plc:zzzzzzzzzzzzzzzzzzzzzzzz", path),
        key,
        now_ms(),
    )
    .await;
    assert!(
        matches!(failure, ProofOutcome::Invalid(ProofFailure::Commit(_))),
        "{failure:?}"
    );
    let other = AtprotoKey::from_multikey("zQ3shpG8vFZ6MmTHNVv7PHmRrw97oArf4QnYJh88zFnNpuYzJ")
        .expect("key");
    let failure = prove_record(car, record_ref(DID, path), other, now_ms()).await;
    match failure {
        ProofOutcome::Invalid(failure) => assert!(failure.needs_identity_refresh()),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn future_rev() {
    let (signing, key) = k256_key();
    let records = records(5);
    let now = now_ms();
    let repo = TestRepo::build(DID, &signing, &records, rev_at(now + 10 * 60 * 1000)).await;
    let (path, _) = &records[0];
    let car = repo.car(path).await;
    assert!(matches!(
        prove_record(car.clone(), record_ref(DID, path), key.clone(), now).await,
        ProofOutcome::Invalid(ProofFailure::Commit(_))
    ));
    // Within the five-minute skew it proves.
    let repo = TestRepo::build(DID, &signing, &records, rev_at(now + 4 * 60 * 1000)).await;
    let car = repo.car(path).await;
    assert!(matches!(
        prove_record(car, record_ref(DID, path), key, now).await,
        ProofOutcome::Present { .. }
    ));
}

#[tokio::test]
async fn proved_absence_and_missing_path_block() {
    let (signing, key) = k256_key();
    let records = records(300);
    let repo = TestRepo::build(DID, &signing, &records, rev_at(now_ms())).await;
    let missing = signature_path("3lbmxyz2abc2k");
    let path = repo.path_blocks(&missing).await;
    assert!(path.len() >= 2, "the tree is too shallow for this test");

    let car = repo.car(&missing).await;
    assert!(matches!(
        prove_record(car, record_ref(DID, &missing), key.clone(), now_ms()).await,
        ProofOutcome::Absent { .. }
    ));

    // Without the last node on the path, absence is not proved: the proof is incomplete,
    // although jacquard-repo's verify_proofs alone would accept the absence claim.
    let dropped = *path.last().expect("node");
    let car = repo
        .car_with(&missing, |blocks| {
            blocks.remove(&dropped);
        })
        .await;
    let did = JDid::<SmolStr>::new_owned(DID).expect("did");
    let claim = RecordClaim {
        collection: CowStr::Owned(SmolStr::from(COLLECTION)),
        rkey: CowStr::Owned(SmolStr::from("3lbmxyz2abc2k")),
        cid: None,
    };
    let output = verify_proofs(&car, vec![claim.clone()], &did, &key.to_jacquard())
        .await
        .expect("jacquard");
    assert!(output.verified.contains(&claim));
    assert!(matches!(
        prove_record(car, record_ref(DID, &missing), key, now_ms()).await,
        ProofOutcome::Incomplete(_)
    ));
}

#[tokio::test]
async fn caps() {
    let (_, key) = k256_key();
    let path = signature_path("3lbmxyz2abc2k");
    let big = vec![0u8; MAX_CAR_BYTES + 1];
    assert!(matches!(
        prove_record(big, record_ref(DID, &path), key.clone(), now_ms()).await,
        ProofOutcome::Invalid(ProofFailure::Car(_))
    ));
    let garbage = b"not a car".to_vec();
    assert!(matches!(
        prove_record(garbage, record_ref(DID, &path), key.clone(), now_ms()).await,
        ProofOutcome::Invalid(ProofFailure::Car(_))
    ));
    // More than 2000 blocks.
    let (signing, _) = k256_key();
    let repo = TestRepo::build(DID, &signing, &records(1), rev_at(now_ms())).await;
    let car = repo
        .car_with(&path, |blocks| {
            for i in 0..MAX_CAR_BLOCKS {
                let block = serde_ipld_dagcbor::to_vec(&(i as u64)).expect("cbor");
                blocks.insert(crate::ipld::cid_for_dag_cbor(&block), Bytes::from(block));
            }
        })
        .await;
    assert!(matches!(
        prove_record(car, record_ref(DID, &path), key, now_ms()).await,
        ProofOutcome::Invalid(ProofFailure::Car(_))
    ));
}

fn fixture(path: &str) -> Vec<u8> {
    let path = format!("{}/testdata/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).expect(&path)
}

fn owner_key() -> AtprotoKey {
    let did = Did::parse("did:web:rawkode.dev", Mode::Production).expect("did");
    crate::document::read_document(&did, &fixture("web/rawkode.dev.did.json"), Mode::Production)
        .expect("document")
        .signing_key
        .expect("key")
}

/// A proof served by a real PDS (the forge owner's, captured 2026-11-09).
#[tokio::test]
async fn captured_presence_proof() {
    let car = fixture("pds/rawkode-profile.sync.getRecord.car");
    let answer: serde_json::Value =
        serde_json::from_slice(&fixture("pds/rawkode-profile.getRecord.json")).expect("json");
    let record = RecordRef {
        did: Did::parse("did:web:rawkode.dev", Mode::Production).expect("did"),
        collection: "app.bsky.actor.profile".into(),
        rkey: "self".into(),
    };
    match prove_record(car, record, owner_key(), now_ms()).await {
        ProofOutcome::Present { record_cid, .. } => {
            assert_eq!(record_cid.to_string(), answer["cid"].as_str().expect("cid"));
        }
        other => panic!("{other:?}"),
    }
}

/// The same PDS's answer for a signature record that does not exist: 200 with a CAR proving the
/// absence, as the reference PDS answers.
#[tokio::test]
async fn captured_absence_proof() {
    let car = fixture("pds/rawkode-absent-signature.sync.getRecord.car");
    let record = RecordRef {
        did: Did::parse("did:web:rawkode.dev", Mode::Production).expect("did"),
        collection: COLLECTION.into(),
        rkey: "3lbmxyz2abc2k".into(),
    };
    assert!(matches!(
        prove_record(car, record, owner_key(), now_ms()).await,
        ProofOutcome::Absent { .. }
    ));
}

#[tokio::test]
async fn judging_signatures() {
    let (signing, key) = k256_key();
    let payload = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n";
    let salt = Salt::from_bytes([21; 32]);
    let record = SignatureRecord::for_git(payload, &salt, "2026-11-09T12:00:00Z".into());
    let rkey = crate::signature::tid::Tid::parse("3lbmxyz2abc2k").expect("tid");
    let path = signature_path(rkey.as_str());
    let mut all = records(20);
    all.push((path.clone(), record.to_dag_cbor()));
    let repo = TestRepo::build(DID, &signing, &all, rev_at(now_ms())).await;
    // The CID computed locally is the CID jacquard-repo's MST stores for the record.
    assert_eq!(repo.mst.get(&path).await.expect("get"), Some(record.cid()));
    let car = repo.car(&path).await;
    let block = SignatureBlock {
        did: Did::parse(DID, Mode::Production).expect("did"),
        rkey,
        cid: record.cid(),
        salt: salt.clone(),
    };
    let outcome = prove_record(car, record_ref(DID, &path), key, now_ms()).await;

    assert!(matches!(
        judge_signature(outcome.clone(), &block, payload),
        SignatureJudgement::Verified { .. }
    ));
    assert!(matches!(
        judge_signature(outcome.clone(), &block, b"tree other\n"),
        SignatureJudgement::Mismatch(_)
    ));
    let mut other_salt = block.clone();
    other_salt.salt = Salt::from_bytes([22; 32]);
    assert!(matches!(
        judge_signature(outcome.clone(), &other_salt, payload),
        SignatureJudgement::Mismatch(_)
    ));
    let mut replaced = block.clone();
    replaced.cid = crate::ipld::cid_for_dag_cbor(b"other");
    assert!(matches!(
        judge_signature(outcome, &replaced, payload),
        SignatureJudgement::Replaced { .. }
    ));
}

#[tokio::test]
async fn fetching_records() {
    let server = TestServer::start(vec![
        Reply::ok(b"car bytes".to_vec()).header("Content-Type", "application/vnd.ipld.car"),
        Reply::json(serde_json::json!({"error": "RecordNotFound"})).status(400),
        Reply::json(serde_json::json!({"error": "RepoDeactivated"})).status(400),
        Reply::ok(Vec::new()).status(503),
        Reply::json(serde_json::json!({"error": "InvalidRequest"})).status(400),
    ])
    .await;
    let fetch =
        FetchClient::with_exempt(FetchConfig::new(Mode::Production), server.addr).expect("client");
    let pds = server.url("");
    let record = record_ref(DID, &signature_path("3lbmxyz2abc2k"));
    assert_eq!(
        fetch_record(&fetch, &pds, &record).await,
        RecordAnswer::Car(b"car bytes".to_vec())
    );
    assert_eq!(
        fetch_record(&fetch, &pds, &record).await,
        RecordAnswer::RecordNotFound
    );
    assert_eq!(
        fetch_record(&fetch, &pds, &record).await,
        RecordAnswer::RepoUnavailable("RepoDeactivated".into())
    );
    assert!(matches!(
        fetch_record(&fetch, &pds, &record).await,
        RecordAnswer::Unavailable(_)
    ));
    assert!(matches!(
        fetch_record(&fetch, &pds, &record).await,
        RecordAnswer::Failed(_)
    ));
    assert_eq!(
        server.requests()[0].path,
        format!(
            "/xrpc/com.atproto.sync.getRecord?did=did%3Aplc%3Aabcdefghijklmnopqrstuvwx&collection={COLLECTION}&rkey=3lbmxyz2abc2k"
        )
    );
}

#[test]
fn bundle_shape() {
    let salt = Salt::from_bytes([1; 32]);
    let record = SignatureRecord::for_git(b"payload", &salt, "2026-11-09T12:00:00Z".into());
    let block = SignatureBlock {
        did: Did::parse(DID, Mode::Production).expect("did"),
        rkey: crate::signature::tid::Tid::parse("3lbmxyz2abc2k").expect("tid"),
        cid: record.cid(),
        salt,
    };
    let resolved = Resolved {
        identity: None,
        evidence: Evidence::Plc {
            audit_log: b"[]".to_vec(),
            operation_cid: "bafyreib".into(),
        },
        fetched_at_ms: 0,
    };
    let (_, key) = k256_key();
    let commit = ProvedCommit {
        cid: crate::ipld::cid_for_dag_cbor(b"commit"),
        rev: "3lbmxyz2abc2k".into(),
    };
    let bundle = ProofBundle::new(BundleInput {
        oid: "0123456789abcdef0123456789abcdef01234567",
        kind: "commit",
        payload: b"payload",
        block: &block,
        resolved: &resolved,
        key: &key,
        commit: &commit,
        car: b"car",
        fetched_at: "2026-11-09T12:00:00Z",
    });
    let json: serde_json::Value = serde_json::from_slice(&bundle.to_json()).expect("json");
    assert_eq!(json["format"], "tartan.signature-proof/2");
    assert_eq!(json["identity"]["method"], "plc");
    assert_eq!(json["identity"]["audit_log"], "W10=");
    assert_eq!(json["record"]["collection"], COLLECTION);
    assert_eq!(
        json["object"]["payload_sha256"],
        hex::encode(Sha256::digest(b"payload"))
    );
    assert!(
        json["atproto_key"]
            .as_str()
            .expect("key")
            .starts_with("did:key:zQ3s")
    );
    let parsed: ProofBundle = serde_json::from_slice(&bundle.to_json()).expect("round trip");
    assert_eq!(parsed, bundle);
    assert_eq!(
        bundle.store_key(),
        format!("proofs/sha256/{}.json", bundle.sha256())
    );
}

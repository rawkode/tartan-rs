//! A person's repository on one PDS host: records in a jacquard-repo MST and a signed commit.
//!
//! The commit is signed when it is served, with the repository's current key or, for a set
//! number of answers after a key rotation, with the key it had before, so tests can see a PDS
//! that has not caught up with a rotation.

use std::collections::BTreeMap;
use std::sync::Arc;

use cid::Cid;
use jacquard_common::deps::bytes::Bytes;
use jacquard_common::deps::smol_str::SmolStr;
use jacquard_common::types::did::Did as JDid;
use jacquard_common::types::tid::Tid as JTid;
use jacquard_repo::commit::Commit;
use jacquard_repo::{BlockStore, MemoryBlockStore, Mst};
use k256::ecdsa::SigningKey;
use serde_json::Value;
use tartan_atproto::ipld::{cid_for_dag_cbor, json_to_dag_cbor};
use tartan_atproto::signature::tid::Tid;

/// A stored record.
#[derive(Debug, Clone)]
pub(crate) struct Record {
    /// The record as JSON (the form XRPC answers and Jetstream carry).
    pub json: Value,
    /// Its CID.
    pub cid: Cid,
}

/// One write of an `applyWrites` batch.
#[derive(Debug, Clone)]
pub(crate) enum Write {
    Create {
        collection: String,
        rkey: String,
        value: Value,
    },
    Update {
        collection: String,
        rkey: String,
        value: Value,
    },
    Delete {
        collection: String,
        rkey: String,
    },
}

impl Write {
    pub(crate) fn path(&self) -> String {
        match self {
            Write::Create {
                collection, rkey, ..
            }
            | Write::Update {
                collection, rkey, ..
            }
            | Write::Delete { collection, rkey } => format!("{collection}/{rkey}"),
        }
    }

    /// The reference PDS's rate-limit cost (create 3, update 2, delete 1).
    pub(crate) fn points(&self) -> u32 {
        match self {
            Write::Create { .. } => 3,
            Write::Update { .. } => 2,
            Write::Delete { .. } => 1,
        }
    }
}

/// What a committed write did, for answers and Jetstream.
#[derive(Debug, Clone)]
pub(crate) struct Applied {
    pub operation: &'static str,
    pub collection: String,
    pub rkey: String,
    pub record: Option<Record>,
}

/// A committed batch.
#[derive(Debug, Clone)]
pub(crate) struct Committed {
    pub commit_cid: Cid,
    pub rev: String,
    pub applied: Vec<Applied>,
}

/// Why a batch was refused. Nothing in the batch is applied.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum WriteError {
    /// A create on an existing key, an update or delete of a missing one: the reference PDS
    /// throws inside its MST and answers 500 `InternalServerError`.
    #[error("{0}")]
    Conflict(String),
    /// A record that cannot be stored (not DAG-CBOR, a bad key).
    #[error("{0}")]
    Invalid(String),
}

/// A repository.
pub(crate) struct Repo {
    did: String,
    records: BTreeMap<String, Record>,
    store: Arc<MemoryBlockStore>,
    /// The persisted MST root. Every operation loads the tree from it afresh, so a refused
    /// batch can never leave changes behind in a shared in-memory tree.
    data: Cid,
    rev: String,
    /// The key commits are signed with.
    pub key: SigningKey,
    /// After a rotation: the previous key and how many more `getRecord` answers use it.
    pub stale: Option<(SigningKey, u32)>,
}

impl Repo {
    /// An empty repository.
    pub(crate) async fn new(did: &str, key: SigningKey) -> Self {
        let store = Arc::new(MemoryBlockStore::new());
        let data = Mst::new(store.clone())
            .persist()
            .await
            .expect("an empty MST persists");
        Repo {
            did: did.to_owned(),
            records: BTreeMap::new(),
            store,
            data,
            rev: Tid::now().as_str().to_owned(),
            key,
            stale: None,
        }
    }

    /// A copy of this repository's records under a new key (an account migration).
    pub(crate) async fn copy_with_key(&self, key: SigningKey) -> Result<Self, WriteError> {
        let mut copy = Repo::new(&self.did, key).await;
        let writes: Vec<Write> = self
            .records
            .iter()
            .map(|(path, record)| {
                let (collection, rkey) = path.split_once('/').unwrap_or((path, ""));
                Write::Create {
                    collection: collection.to_owned(),
                    rkey: rkey.to_owned(),
                    value: record.json.clone(),
                }
            })
            .collect();
        if !writes.is_empty() {
            copy.apply(&writes).await?;
        }
        Ok(copy)
    }

    /// The record at `collection/rkey`.
    pub(crate) fn get(&self, collection: &str, rkey: &str) -> Option<&Record> {
        self.records.get(&format!("{collection}/{rkey}"))
    }

    /// The records of `collection` in record-key order.
    pub(crate) fn list(&self, collection: &str) -> Vec<(String, Record)> {
        let prefix = format!("{collection}/");
        self.records
            .range(prefix.clone()..)
            .take_while(|(path, _)| path.starts_with(&prefix))
            .map(|(path, record)| (path[prefix.len()..].to_owned(), record.clone()))
            .collect()
    }

    /// The current revision.
    pub(crate) fn rev(&self) -> &str {
        &self.rev
    }

    /// Applies `writes` atomically and commits.
    pub(crate) async fn apply(&mut self, writes: &[Write]) -> Result<Committed, WriteError> {
        let mut mst = Mst::load(self.store.clone(), self.data, None);
        let mut records = self.records.clone();
        let mut applied = Vec::with_capacity(writes.len());
        for write in writes {
            let path = write.path();
            let exists = records.contains_key(&path);
            match write {
                Write::Create {
                    collection,
                    rkey,
                    value,
                }
                | Write::Update {
                    collection,
                    rkey,
                    value,
                } => {
                    let create = matches!(write, Write::Create { .. });
                    if create && exists {
                        return Err(WriteError::Conflict(format!(
                            "There is already a value at key: {path}"
                        )));
                    }
                    if !create && !exists {
                        return Err(WriteError::Conflict(format!(
                            "Could not find record: {path}"
                        )));
                    }
                    let block = json_to_dag_cbor(value)
                        .map_err(|error| WriteError::Invalid(error.to_string()))?;
                    let cid = self
                        .store
                        .put(&block)
                        .await
                        .map_err(|error| WriteError::Invalid(error.to_string()))?;
                    debug_assert_eq!(cid, cid_for_dag_cbor(&block));
                    mst = if create {
                        mst.add(&path, cid).await
                    } else {
                        mst.update(&path, cid).await
                    }
                    .map_err(|error| WriteError::Invalid(error.to_string()))?;
                    let record = Record {
                        json: value.clone(),
                        cid,
                    };
                    records.insert(path, record.clone());
                    applied.push(Applied {
                        operation: if create { "create" } else { "update" },
                        collection: collection.clone(),
                        rkey: rkey.clone(),
                        record: Some(record),
                    });
                }
                Write::Delete { collection, rkey } => {
                    if !exists {
                        return Err(WriteError::Conflict(format!(
                            "Could not find record: {path}"
                        )));
                    }
                    mst = mst
                        .delete(&path)
                        .await
                        .map_err(|error| WriteError::Invalid(error.to_string()))?;
                    records.remove(&path);
                    applied.push(Applied {
                        operation: "delete",
                        collection: collection.clone(),
                        rkey: rkey.clone(),
                        record: None,
                    });
                }
            }
        }
        let data = mst
            .persist()
            .await
            .map_err(|error| WriteError::Invalid(error.to_string()))?;
        self.records = records;
        self.data = data;
        self.rev = next_rev(&self.rev);
        let commit_cid = self.signed_commit(&self.key).1;
        Ok(Committed {
            commit_cid,
            rev: self.rev.clone(),
            applied,
        })
    }

    fn signed_commit(&self, key: &SigningKey) -> (Commit<SmolStr>, Cid) {
        let commit = Commit::new_unsigned(
            JDid::<SmolStr>::new_owned(&self.did).expect("the devnet's DIDs are valid"),
            self.data,
            JTid::new(&self.rev).expect("revisions are TIDs"),
            None,
        )
        .sign(key)
        .expect("a k256 key signs");
        let cid = commit.to_cid().expect("a commit encodes");
        (commit, cid)
    }

    /// The `com.atproto.sync.getRecord` CAR for `collection/rkey`: the commit, the MST path and
    /// the record block when it exists (a non-existence proof otherwise). With `drop_path_block`
    /// the deepest MST node of the path is left out, so the proof is incomplete.
    pub(crate) async fn car(
        &mut self,
        collection: &str,
        rkey: &str,
        drop_path_block: bool,
    ) -> Result<Vec<u8>, WriteError> {
        let key = match &mut self.stale {
            Some((old, remaining)) if *remaining > 0 => {
                *remaining -= 1;
                old.clone()
            }
            _ => {
                self.stale = None;
                self.key.clone()
            }
        };
        let (commit, commit_cid) = self.signed_commit(&key);
        let path = format!("{collection}/{rkey}");
        let mut cids = Mst::load(self.store.clone(), self.data, None)
            .cids_for_path(&path)
            .await
            .map_err(|error| WriteError::Invalid(error.to_string()))?;
        if drop_path_block {
            let leaf = self.records.get(&path).map(|record| record.cid);
            if let Some(index) = cids.iter().rposition(|cid| Some(*cid) != leaf) {
                cids.remove(index);
            }
        }
        let mut blocks = BTreeMap::new();
        blocks.insert(
            commit_cid,
            Bytes::from(commit.to_cbor().expect("a commit encodes")),
        );
        for cid in cids {
            if let Some(block) = self
                .store
                .get(&cid)
                .await
                .map_err(|error| WriteError::Invalid(error.to_string()))?
            {
                blocks.insert(cid, block);
            }
        }
        jacquard_repo::car::write_car_bytes(commit_cid, blocks)
            .await
            .map_err(|error| WriteError::Invalid(error.to_string()))
    }
}

/// A TID later than `previous` and not earlier than now.
fn next_rev(previous: &str) -> String {
    let now = Tid::now();
    match Tid::parse(previous) {
        Ok(previous) if previous.micros() >= now.micros() => {
            Tid::from_parts(previous.micros() + 1, 0)
                .as_str()
                .to_owned()
        }
        _ => now.as_str().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tartan_atproto::Mode;
    use tartan_atproto::did::Did;
    use tartan_atproto::proof::{ProofOutcome, RecordRef, prove_record};

    const DID: &str = "did:plc:abcdefghijklmnopqrstuvwx";

    fn record_ref(rkey: &str) -> RecordRef {
        RecordRef {
            did: Did::parse(DID, Mode::Development).expect("did"),
            collection: "app.test.thing".into(),
            rkey: rkey.into(),
        }
    }

    async fn repo_with(count: usize) -> Repo {
        let mut repo = Repo::new(DID, crate::keys::derive("t", "repo")).await;
        let writes: Vec<Write> = (0..count)
            .map(|i| Write::Create {
                collection: "app.test.thing".into(),
                rkey: format!("k{i:04}"),
                value: json!({"$type": "app.test.thing", "n": i}),
            })
            .collect();
        repo.apply(&writes).await.expect("apply");
        repo
    }

    #[tokio::test]
    async fn proofs_of_presence_and_absence() {
        let mut repo = repo_with(40).await;
        let key = crate::keys::public(&repo.key);
        let car = repo
            .car("app.test.thing", "k0007", false)
            .await
            .expect("car");
        assert!(matches!(
            prove_record(car, record_ref("k0007"), key.clone(), crate::util::now_ms()).await,
            ProofOutcome::Present { .. }
        ));
        let car = repo.car("app.test.thing", "zzz", false).await.expect("car");
        assert!(matches!(
            prove_record(car, record_ref("zzz"), key.clone(), crate::util::now_ms()).await,
            ProofOutcome::Absent { .. }
        ));
        let car = repo.car("app.test.thing", "zzz", true).await.expect("car");
        assert!(matches!(
            prove_record(car, record_ref("zzz"), key, crate::util::now_ms()).await,
            ProofOutcome::Incomplete(_)
        ));
    }

    #[tokio::test]
    async fn batches_are_atomic() {
        let mut repo = repo_with(1).await;
        let rev = repo.rev().to_owned();
        let error = repo
            .apply(&[
                Write::Create {
                    collection: "app.test.thing".into(),
                    rkey: "new".into(),
                    value: json!({"n": 1}),
                },
                Write::Create {
                    collection: "app.test.thing".into(),
                    rkey: "k0000".into(),
                    value: json!({"n": 2}),
                },
            ])
            .await;
        assert!(matches!(error, Err(WriteError::Conflict(_))));
        assert!(repo.get("app.test.thing", "new").is_none());
        assert_eq!(repo.rev(), rev);
    }

    #[tokio::test]
    async fn stale_key_serves_a_set_number_of_answers() {
        let mut repo = repo_with(3).await;
        let old = repo.key.clone();
        repo.key = crate::keys::derive("t", "new");
        repo.stale = Some((old.clone(), 1));
        let first = repo
            .car("app.test.thing", "k0001", false)
            .await
            .expect("car");
        let second = repo
            .car("app.test.thing", "k0001", false)
            .await
            .expect("car");
        let (old_key, new_key) = (crate::keys::public(&old), crate::keys::public(&repo.key));
        let now = crate::util::now_ms();
        assert!(matches!(
            prove_record(first, record_ref("k0001"), old_key, now).await,
            ProofOutcome::Present { .. }
        ));
        assert!(matches!(
            prove_record(second, record_ref("k0001"), new_key, now).await,
            ProofOutcome::Present { .. }
        ));
    }
}

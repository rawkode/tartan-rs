//! The proof-reuse cache (design D8 step 5): a `getRecord` answer fetched for a record is
//! reused for at most ten minutes by later checks of the same record in this process.
//!
//! It lives in memory in each process, so dropping an entry is best effort: correctness never
//! rests on a drop but on confirming every revocation with a fresh fetch (design D7,
//! *Confirming before revoking*) and on `SignerStanding`. Entries are dropped for a DID when an
//! identity or account event arrives for it or a refresh finds its `#atproto` key or PDS
//! changed, and for a record when a Jetstream `delete` or `update` arrives for it. A re-proof
//! (`ReproveSignature`) never reads it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// How long a cached proof may be reused.
pub const REUSE_MS: i64 = 10 * 60 * 1000;
/// The most entries kept; beyond it, expired entries are dropped, then the oldest.
const MAX_ENTRIES: usize = 10_000;

/// A `getRecord` answer and the evidence it was proved against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedProof {
    /// The PDS it came from.
    pub pds: String,
    /// The `#atproto` key (a `did:key`) it was proved against.
    pub atproto_key: String,
    /// The CAR as fetched.
    pub car: Arc<Vec<u8>>,
    /// When it was fetched (clock milliseconds).
    pub fetched_at_ms: i64,
}

/// The cache, keyed by `(did, rkey)`.
#[derive(Debug, Default)]
pub struct ProofCache {
    entries: Mutex<HashMap<(String, String), CachedProof>>,
}

impl ProofCache {
    fn lock(&self) -> MutexGuard<'_, HashMap<(String, String), CachedProof>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The proof of `(did, rkey)` fetched less than [`REUSE_MS`] before `now_ms`, if any.
    pub fn get(&self, did: &str, rkey: &str, now_ms: i64) -> Option<CachedProof> {
        let entries = self.lock();
        entries
            .get(&(did.to_owned(), rkey.to_owned()))
            .filter(|proof| now_ms - proof.fetched_at_ms < REUSE_MS)
            .cloned()
    }

    /// Keeps `proof` for `(did, rkey)`.
    pub fn put(&self, did: &str, rkey: &str, proof: CachedProof) {
        let mut entries = self.lock();
        if entries.len() >= MAX_ENTRIES {
            let now = proof.fetched_at_ms;
            entries.retain(|_, kept| now - kept.fetched_at_ms < REUSE_MS);
            if entries.len() >= MAX_ENTRIES
                && let Some(oldest) = entries
                    .iter()
                    .min_by_key(|(_, kept)| kept.fetched_at_ms)
                    .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert((did.to_owned(), rkey.to_owned()), proof);
    }

    /// Drops every cached proof of `did`.
    pub fn drop_did(&self, did: &str) {
        self.lock().retain(|(cached, _), _| cached != did);
    }

    /// Drops the cached proof of `(did, rkey)`.
    pub fn drop_record(&self, did: &str, rkey: &str) {
        self.lock().remove(&(did.to_owned(), rkey.to_owned()));
    }

    /// How many entries are cached.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// This process's cache.
pub fn proof_cache() -> &'static ProofCache {
    static CACHE: OnceLock<ProofCache> = OnceLock::new();
    CACHE.get_or_init(ProofCache::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proof(at: i64) -> CachedProof {
        CachedProof {
            pds: "https://pds.example".into(),
            atproto_key: "did:key:z".into(),
            car: Arc::new(vec![1, 2, 3]),
            fetched_at_ms: at,
        }
    }

    #[test]
    fn reuse_window_and_drops() {
        let cache = ProofCache::default();
        cache.put("did:a", "r1", proof(1_000));
        cache.put("did:a", "r2", proof(1_000));
        cache.put("did:b", "r1", proof(1_000));
        assert!(cache.get("did:a", "r1", 1_000 + REUSE_MS - 1).is_some());
        assert!(cache.get("did:a", "r1", 1_000 + REUSE_MS).is_none());
        cache.drop_record("did:a", "r1");
        assert!(cache.get("did:a", "r1", 1_000).is_none());
        assert!(cache.get("did:a", "r2", 1_000).is_some());
        cache.drop_did("did:a");
        assert!(cache.get("did:a", "r2", 1_000).is_none());
        assert_eq!(cache.len(), 1);
    }
}

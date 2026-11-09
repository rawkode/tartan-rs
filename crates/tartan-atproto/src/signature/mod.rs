//! The Tartan signature formats shared by the forge and `tt` (design D7).
//!
//! - [`record`]: the `academy.rawkode.tartan.signature` record, its keyed digest, DAG-CBOR
//!   encoding and CID.
//! - [`block`]: the six-line signature Git stores, its writer and its strict parser.
//! - [`tid`]: TID record keys.
//! - [`git`]: extracting the signature and the signed payload from raw Git objects, and
//!   classifying the signature.

pub mod block;
pub mod git;
pub mod record;
pub mod tid;

/// The NSID of the signature record.
pub const COLLECTION: &str = "academy.rawkode.tartan.signature";

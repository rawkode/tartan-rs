//! ATProto identity for Tartan.
//!
//! This crate holds the identity work shared by the forge (`tartan-forge`), the `tt` command-line
//! interface and the development network (`tartan-devnet`): DID and handle resolution with
//! evidence, PLC log verification, record-proof verification and the Tartan signature format.
//! It never depends on the forge's actors or its extension runtime, so `tt` can link it without
//! wasmtime.
//!
//! - [`did`]: DID syntax (`did:plc` and hostname-only `did:web`).
//! - [`fetch`]: the one outbound HTTP client every identity fetch uses (design D9, *Fetch
//!   safety*).
//! - [`keys`]: `#atproto` keys and low-S signature verification.
//! - [`plc`]: verifying a `did:plc` audit log and deriving the DID document from it.
//! - [`document`]: reading a `did:web` DID document.
//! - [`handle`]: handle syntax and bidirectional handle verification.
//! - [`resolver`]: [`resolver::VerifyingResolver`], which resolves DIDs with evidence and
//!   implements jacquard's resolver traits.
//! - [`proof`]: proving a record against a repository commit (`prove_record`) and the proof
//!   bundle.
//! - [`ipld`]: JSON to DAG-CBOR conversion and CIDs.
//! - [`signature`]: the `academy.rawkode.tartan.signature` record, the six-line signature
//!   block, TIDs and signature extraction from Git objects.

pub mod did;
pub mod document;
pub mod fetch;
pub mod handle;
pub mod ipld;
pub mod keys;
pub mod plc;
pub mod proof;
pub mod resolver;
pub mod signature;

#[cfg(test)]
mod testutil;

/// Whether the development identity switches apply.
///
/// [`Mode::Production`] is the only mode a build without the `dev-identity` feature has. The
/// binaries choose [`Mode::Development`] only when the feature is compiled in *and*
/// `TARTAN_DEV_IDENTITY=1` is set (design D12); this crate never reads the environment itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Public HTTPS only, public addresses only, `plc.directory` and DNS.
    #[default]
    Production,
    /// Plain HTTP and private addresses are allowed, `localhost` did:web hosts may carry a
    /// port, and every name under `localhost.` resolves to the loopback address.
    #[cfg(feature = "dev-identity")]
    Development,
}

impl Mode {
    /// True in [`Mode::Development`].
    pub fn is_development(self) -> bool {
        match self {
            Mode::Production => false,
            #[cfg(feature = "dev-identity")]
            Mode::Development => true,
        }
    }
}

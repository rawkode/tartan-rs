//! ATProto identity for Tartan.
//!
//! This crate holds the identity work shared by the forge (`tartan-forge`), the `tt` command-line
//! interface and the development network (`tartan-devnet`): DID and handle resolution with
//! evidence, PLC log verification, record-proof verification and the Tartan signature format.
//! It never depends on the forge's actors or its extension runtime, so `tt` can link it without
//! wasmtime.

//! A local ATProto network for development and tests.
//!
//! One in-memory process serves a PLC directory, a PDS, an OAuth authorization server and a
//! Jetstream-compatible firehose, with people derived deterministically from a seed, so the
//! forge's identity and signing paths run end to end without the public network.

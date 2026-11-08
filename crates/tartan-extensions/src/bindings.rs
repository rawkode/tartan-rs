//! Bindings generated from `wit/tartan.wit`.

// The generated code has no doc comments of its own.
#![allow(missing_docs)]

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "extension",
    imports: { default: async | trappable },
    exports: { default: async },
});

pub(crate) use self::tartan::ext::{effects, host, kv, sql, types};

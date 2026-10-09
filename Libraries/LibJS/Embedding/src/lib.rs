/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Access to the LibJS runtime for embedders written in Rust, through the runtime's embedding ABI, the same C
//! interface that LibJS's C++ API is built on.

#[allow(
    clippy::all,
    clippy::pedantic,
    non_camel_case_types,
    non_upper_case_globals,
    reason = "generated from the embedding module of libjs_rust"
)]
pub mod abi {
    include!(concat!(env!("OUT_DIR"), "/abi.rs"));

    /// Definitions of every function of the ABI as one that must never run, for test binaries that do not link LibJS.
    #[cfg(any(test, feature = "test-stubs"))]
    mod stubs {
        include!(concat!(env!("OUT_DIR"), "/abi_stubs.rs"));
    }
}

/// The offsets, sizes and constants of the runtime's structures that an embedder reads or writes directly.
#[allow(
    clippy::all,
    clippy::pedantic,
    reason = "generated from the layout module of libjs_rust"
)]
pub mod layout {
    include!(concat!(env!("OUT_DIR"), "/layout.rs"));
}

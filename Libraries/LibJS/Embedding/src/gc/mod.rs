/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Cells that an embedder defines in Rust, which LibGC allocates, traces and destroys through its C API.

pub mod capi;
pub mod cell;
pub mod visitor;

pub use cell::{CellType, Gc};
/// Implements Trace by tracing every field; see libjs_runtime_macros. Use it as `#[derive(Trace)]` with
/// `#[gc(crate = libjs_embedding)]`.
pub use libjs_runtime_macros::Trace;
pub use visitor::{Trace, Visitor};

#[cfg(test)]
mod tests {
    use super::{Gc, Trace};
    use crate::value::Value;

    #[derive(Trace)]
    #[gc(crate = crate)]
    #[allow(dead_code, reason = "the type only has to compile")]
    struct Traced {
        value: Value,
        cells: Vec<Gc<u32>>,
        #[gc(untraced)]
        count: core::cell::Cell<u64>,
    }

    #[test]
    fn derived_trace_compiles() {
        fn assert_trace<T: super::visitor::Trace>() {}
        assert_trace::<Traced>();
    }
}

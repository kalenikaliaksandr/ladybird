/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Module records: creating them, loading, linking and evaluating module graphs, the module requests that
//! HostLoadImportedModule receives and FinishLoadingImportedModule completes, and module namespaces.
//!
//! The exported functions run on the thread that owns the VM and trust their arguments, as object.rs states: `vm` is
//! the embedder's VM; modules, realms and the records of referrers and payloads are live cells of its heap;
//! host-defined cells are null or live cells of its heap; module requests are ones the runtime lent or the embedder
//! owns; and views and out parameters are valid. Cells these functions return are not rooted.
//!
//! Loading, linking and evaluating create promises and run JavaScript, so like HTML, which prepares to run script
//! first, the embedder calls them with an execution context of the module's realm running.

use crate::embedding::abi_types::CellAbi;
use crate::layout::cell::Gc;
use crate::layout::host_class::{JSModule, JSPromiseCapability};
use crate::runtime::module::Module;
use crate::runtime::promise_capability::PromiseCapability;

impl CellAbi for JSModule {
    type Cell = Module;
}

pub(crate) fn promise_capability_into_abi(capability: Gc<PromiseCapability>) -> *mut JSPromiseCapability {
    capability.as_ptr().cast()
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::layout::vm::VmHead;

/// The virtual machine the interpreter runs on. The interpreter reads and writes the head directly.
#[repr(C)]
pub struct Vm {
    pub head: VmHead,
}

const _: () = assert!(core::mem::offset_of!(Vm, head) == 0);

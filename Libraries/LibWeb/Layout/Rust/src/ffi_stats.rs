/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::abort_on_panic;
use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! define_ffi_ops {
    ($($variant:ident => $name:literal,)+) => {
        #[derive(Clone, Copy)]
        #[repr(usize)]
        pub(crate) enum FfiOp {
            $($variant,)+
        }

        const FFI_OP_COUNT: usize = 0 $(+ { let _ = FfiOp::$variant; 1 })+;
        static FFI_OP_NAMES: [&str; FFI_OP_COUNT] = [$(concat!($name, "\0"),)+];
    };
}

define_ffi_ops! {
    StateCreate => "stateCreateEntries",
    StateDestroy => "stateDestroyEntries",
    StateEnsureCapacity => "stateEnsureCapacityEntries",
    UsedValuesCreate => "usedValuesCreateEntries",
    UsedValuesGet => "usedValuesGetEntries",
    AbsposRegister => "absposRegisterEntries",
    AbsposTake => "absposTakeEntries",
    AbsposLayoutIndexCallback => "absposLayoutIndexCallbacks",
    StyleFactsBuild => "styleFactsBuildEntries",
    CalcHandleRetain => "calcHandleRetainEntries",
    CalcHandleRelease => "calcHandleReleaseEntries",
    BoxFactsBuild => "boxFactsBuildEntries",
    FcTypeDecision => "fcTypeDecisionEntries",
}

static COUNTERS: [AtomicU64; FFI_OP_COUNT] = [const { AtomicU64::new(0) }; FFI_OP_COUNT];

#[inline]
pub(crate) fn bump(op: FfiOp) {
    COUNTERS[op as usize].fetch_add(1, Ordering::Relaxed);
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_style_facts_build() {
    abort_on_panic(|| bump(FfiOp::StyleFactsBuild));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_calc_handle_retain() {
    abort_on_panic(|| bump(FfiOp::CalcHandleRetain));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_calc_handle_release() {
    abort_on_panic(|| bump(FfiOp::CalcHandleRelease));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_box_facts_build() {
    abort_on_panic(|| bump(FfiOp::BoxFactsBuild));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_counter_count() -> usize {
    abort_on_panic(|| FFI_OP_COUNT)
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_counter_name(index: usize) -> *const u8 {
    abort_on_panic(|| FFI_OP_NAMES[index].as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_counter_value(index: usize) -> u64 {
    abort_on_panic(|| COUNTERS[index].load(Ordering::Relaxed))
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_counters_reset() {
    abort_on_panic(|| {
        for counter in &COUNTERS {
            counter.store(0, Ordering::Relaxed);
        }
    });
}

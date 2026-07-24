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
    AbsposEngine => "absposEngineEntries",
    AbsposReplay => "absposReplayEntries",
    AbsposInlineCbAttempt => "absposInlineCbAttempts",
    AbsposInlineCbNormalFragment => "absposInlineCbNormalFragments",
    AbsposInlineCbAtomicFragment => "absposInlineCbAtomicFragments",
    AbsposInlineCbSuccess => "absposInlineCbSuccesses",
    AbsposSavedInputsGetCallback => "absposSavedInputsGetCallbacks",
    AbsposSavedInputsSetCallback => "absposSavedInputsSetCallbacks",
    AbsposAnchorResolve => "absposAnchorResolveEntries",
    AbsposAnchorLookupCallback => "absposAnchorLookupCallbacks",
    AbsposAnchorFactsCallback => "absposAnchorFactsCallbacks",
    AbsposAnchorFallbackCallback => "absposAnchorFallbackCallbacks",
    AbsposSetResolvedInsetsCallback => "absposSetResolvedInsetsCallbacks",
    AbsposSetScrollShiftCallback => "absposSetScrollShiftCallbacks",
    AbsposAutomaticBlockSizeCallback => "absposAutomaticBlockSizeCallbacks",
    AbsposButtonDefiniteCallback => "absposButtonDefiniteCallbacks",
    StyleFactsBuild => "styleFactsBuildEntries",
    CalcHandleRetain => "calcHandleRetainEntries",
    CalcHandleRelease => "calcHandleReleaseEntries",
    BoxFactsBuild => "boxFactsBuildEntries",
    FcTypeDecision => "fcTypeDecisionEntries",
    TableFactsBuild => "tableFactsBuildEntries",
    GridFactsBuild => "gridFactsBuildEntries",
    GridNameRetain => "gridNameRetainEntries",
    GridNameRelease => "gridNameReleaseEntries",
    AnchorNameRetain => "anchorNameRetainEntries",
    AnchorNameRelease => "anchorNameReleaseEntries",
    SvgFactsBuild => "svgFactsBuildEntries",
    SvgPathRetain => "svgPathRetainEntries",
    SvgPathRelease => "svgPathReleaseEntries",
    TextFactsBuild => "textFactsBuildEntries",
    TextFactsRelease => "textFactsReleaseEntries",
    TextBidiProbeCallback => "textBidiProbeCallbacks",
    DocumentCursorProbeCallback => "documentCursorProbeCallbacks",
    ShapeTextCallback => "shapeTextCallbacks",
    ShapedRunRelease => "shapedRunReleaseEntries",
    FontMetricsCallback => "fontMetricsCallbacks",
    FontGlyphWidthCallback => "fontGlyphWidthCallbacks",
    FontGlyphIdCallback => "fontGlyphIdCallbacks",
    ParentBfcIntrusionCallback => "parentBfcIntrusionCallbacks",
    ParentBfcNextFloatBandCallback => "parentBfcNextFloatBandCallbacks",
    ParentBfcPendingMarginAdjustmentCallback => "parentBfcPendingMarginAdjustmentCallbacks",
    ParentBfcGreatestInlineSizeCallback => "parentBfcGreatestInlineSizeCallbacks",
    ParentBfcClearFloatsCallback => "parentBfcClearFloatsCallbacks",
    ParentBfcResetMarginCallback => "parentBfcResetMarginCallbacks",
    ParentBfcCommitMarginCallback => "parentBfcCommitMarginCallbacks",
    ParentBfcInterruptingBlockCallback => "parentBfcInterruptingBlockCallbacks",
    ParentBfcLayoutFloatCallback => "parentBfcLayoutFloatCallbacks",
    ParentBfcResolveBlockSizeCallback => "parentBfcResolveBlockSizeCallbacks",
    ParentBfcDimensionMarkerCallback => "parentBfcDimensionMarkerCallbacks",
    ParentBfcMarkerDistanceCallback => "parentBfcMarkerDistanceCallbacks",
    LineDrain => "lineDrainEntries",
    FcCreate => "fcCreateEntries",
    FcDestroy => "fcDestroyEntries",
    FcRun => "fcRunEntries",
    FcAutomaticInlineSize => "fcAutomaticInlineSizeEntries",
    FcAutomaticBlockSize => "fcAutomaticBlockSizeEntries",
    FcRunUntilTableInlineSize => "fcRunUntilTableInlineSizeEntries",
    NavigationCallback => "navigationCallbacks",
    UsedValuesCreateCallback => "usedValuesCreateCallbacks",
    UsedValuesGetCallback => "usedValuesGetCallbacks",
    LayoutInsideCallback => "layoutInsideCallbacks",
    ParentDidDimensionCallback => "parentDidDimensionCallbacks",
    DiscardChildContextCallback => "discardChildContextCallbacks",
    IntrinsicMinInlineCallback => "intrinsicMinInlineCallbacks",
    IntrinsicMaxInlineCallback => "intrinsicMaxInlineCallbacks",
    IntrinsicMinBlockCallback => "intrinsicMinBlockCallbacks",
    IntrinsicMaxBlockCallback => "intrinsicMaxBlockCallbacks",
    SetTableCellCoordinatesCallback => "setTableCellCoordinatesCallbacks",
    SetOverrideBordersCallback => "setOverrideBordersCallbacks",
    PlaceChildCallback => "placeChildCallbacks",
    BoxBaselineCallback => "boxBaselineCallbacks",
    ComputeBaselinesCallback => "computeBaselinesCallbacks",
    LayoutAbsposChildrenCallback => "layoutAbsposChildrenCallbacks",
    CaptionLayoutCallback => "captionLayoutCallbacks",
    CellMeasurementCallback => "cellMeasurementCallbacks",
    ShouldTreatMaxInlineAsNoneCallback => "shouldTreatMaxInlineAsNoneCallbacks",
    CalculateInnerInlineSizeCallback => "calculateInnerInlineSizeCallbacks",
    MeasurementStateCreateCallback => "measurementStateCreateCallbacks",
    MeasurementStateDestroyCallback => "measurementStateDestroyCallbacks",
    MeasurementContextRunCallback => "measurementContextRunCallbacks",
    IntrinsicCacheGetCallback => "intrinsicCacheGetCallbacks",
    IntrinsicCachePutCallback => "intrinsicCachePutCallbacks",
    IntrinsicCacheHit => "intrinsicCacheHits",
    StyleFactsCacheHit => "styleFactsCacheHits",
    BoxFactsCacheHit => "boxFactsCacheHits",
    TableFactsCacheHit => "tableFactsCacheHits",
    GridFactsCacheHit => "gridFactsCacheHits",
}

static COUNTERS: [AtomicU64; FFI_OP_COUNT] = [const { AtomicU64::new(0) }; FFI_OP_COUNT];

#[inline]
pub(crate) fn bump(op: FfiOp) {
    COUNTERS[op as usize].fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn fact_build_counts() -> (u64, u64) {
    (
        COUNTERS[FfiOp::StyleFactsBuild as usize].load(Ordering::Relaxed),
        COUNTERS[FfiOp::BoxFactsBuild as usize].load(Ordering::Relaxed),
    )
}

pub(crate) fn exclude_inline_root_fact_builds(before: (u64, u64)) {
    let after = fact_build_counts();
    COUNTERS[FfiOp::StyleFactsBuild as usize].fetch_sub(after.0 - before.0, Ordering::Relaxed);
    COUNTERS[FfiOp::BoxFactsBuild as usize].fetch_sub(after.1 - before.1, Ordering::Relaxed);
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
pub extern "C" fn rust_layout_ffi_note_table_facts_build() {
    abort_on_panic(|| bump(FfiOp::TableFactsBuild));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_grid_facts_build() {
    abort_on_panic(|| bump(FfiOp::GridFactsBuild));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_grid_name_retain() {
    abort_on_panic(|| bump(FfiOp::GridNameRetain));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_grid_name_release() {
    abort_on_panic(|| bump(FfiOp::GridNameRelease));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_anchor_name_retain() {
    abort_on_panic(|| bump(FfiOp::AnchorNameRetain));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_anchor_name_release() {
    abort_on_panic(|| bump(FfiOp::AnchorNameRelease));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_svg_facts_build() {
    abort_on_panic(|| bump(FfiOp::SvgFactsBuild));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_svg_path_retain() {
    abort_on_panic(|| bump(FfiOp::SvgPathRetain));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_svg_path_release() {
    abort_on_panic(|| bump(FfiOp::SvgPathRelease));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_text_facts_build() {
    abort_on_panic(|| bump(FfiOp::TextFactsBuild));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_text_facts_release() {
    abort_on_panic(|| bump(FfiOp::TextFactsRelease));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_text_bidi_probe_callback() {
    abort_on_panic(|| bump(FfiOp::TextBidiProbeCallback));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_document_cursor_probe_callback() {
    abort_on_panic(|| bump(FfiOp::DocumentCursorProbeCallback));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_shape_text_callback() {
    abort_on_panic(|| bump(FfiOp::ShapeTextCallback));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_shaped_run_release() {
    abort_on_panic(|| bump(FfiOp::ShapedRunRelease));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_font_metrics_callback() {
    abort_on_panic(|| bump(FfiOp::FontMetricsCallback));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_font_glyph_width_callback() {
    abort_on_panic(|| bump(FfiOp::FontGlyphWidthCallback));
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_ffi_note_font_glyph_id_callback() {
    abort_on_panic(|| bump(FfiOp::FontGlyphIdCallback));
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

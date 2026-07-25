/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::box_facts::FfiLayoutBoxFacts;
use crate::ffi_stats::{FfiOp, bump};
use crate::formatting_context::grid::facts::GridStyleFacts;
use crate::formatting_context::inline::iterator::text::TextNodeFacts;
use crate::formatting_context::inline::{
    FfiCommittedFragment, FfiInlineBoxPiece, FfiLineRecord, FfiLineSinkCallbacks, line_box::LineBoxData,
    pieces::InlineBoxPieceData, push_line_data,
};
use crate::formatting_context::{FfiBordersData, FfiLayoutFcCallbacks, FfiTableBoxFacts};
use crate::geometry::{FfiContainingBlockConstraints, LogicalRect};
use crate::style_facts::{FfiStyleField, LazyStyleCache, StyleValues};
use crate::used_values::UsedValuesCore;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr::null_mut;

const PAGE_BITS: usize = 4;
const PAGE_SIZE: usize = 1 << PAGE_BITS;
const PAGE_MASK: usize = PAGE_SIZE - 1;
const BUMP_ARENA_CHUNK_SIZE: usize = 4 * 1024;

struct ArenaChunk<T> {
    entries: Box<[MaybeUninit<T>]>,
    initialized: usize,
}

impl<T> ArenaChunk<T> {
    fn new() -> Self {
        let entry_count = (BUMP_ARENA_CHUNK_SIZE / size_of::<T>()).max(1);
        let entries = std::iter::repeat_with(MaybeUninit::uninit)
            .take(entry_count)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            entries,
            initialized: 0,
        }
    }

    fn is_full(&self) -> bool {
        self.initialized == self.entries.len()
    }

    fn allocate(&mut self, value: T) -> *mut T {
        assert!(!self.is_full());
        let entry = &mut self.entries[self.initialized];
        self.initialized += 1;
        entry.write(value)
    }
}

impl<T> Drop for ArenaChunk<T> {
    fn drop(&mut self) {
        for entry in &mut self.entries[..self.initialized] {
            // SAFETY: The prefix ending at `initialized` is written exactly
            // once by allocate(), and no entry is moved out of the arena.
            unsafe {
                entry.assume_init_drop();
            }
        }
    }
}

struct BumpArena<T> {
    chunks: Vec<ArenaChunk<T>>,
}

impl<T> Default for BumpArena<T> {
    fn default() -> Self {
        Self { chunks: Vec::new() }
    }
}

impl<T> BumpArena<T> {
    fn allocate(&mut self, value: T) -> *mut T {
        if self.chunks.last().is_none_or(ArenaChunk::is_full) {
            self.chunks.push(ArenaChunk::new());
        }
        self.chunks.last_mut().unwrap().allocate(value)
    }
}

pub(crate) struct Page<T> {
    entries: [*mut T; PAGE_SIZE],
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self {
            entries: [null_mut(); PAGE_SIZE],
        }
    }
}

pub(crate) struct PagedStore<T> {
    pub(crate) pages: Vec<Option<Box<Page<T>>>>,
    arena: BumpArena<T>,
}

impl<T> Default for PagedStore<T> {
    fn default() -> Self {
        Self {
            pages: Vec::new(),
            arena: BumpArena::default(),
        }
    }
}

impl<T> PagedStore<T> {
    pub(crate) fn ensure_capacity(&mut self, count: u32) {
        let page_count = ((count as usize) + PAGE_SIZE - 1) >> PAGE_BITS;
        self.pages.resize_with(page_count, || None);
    }

    pub(crate) fn get(&self, index: u32) -> *mut T {
        let index = index as usize;
        let page_index = index >> PAGE_BITS;
        let Some(Some(page)) = self.pages.get(page_index) else {
            return null_mut();
        };
        page.entries[index & PAGE_MASK]
    }

    pub(crate) fn allocate(&mut self, index: u32, value: T) -> *mut T {
        let index = index as usize;
        let page_index = index >> PAGE_BITS;
        if page_index >= self.pages.len() {
            self.pages.resize_with(page_index + 1, || None);
        }
        let page = self.pages[page_index].get_or_insert_with(|| Box::new(Page::default()));
        let entry = &mut page.entries[index & PAGE_MASK];
        assert!(entry.is_null());
        *entry = self.arena.allocate(value);
        *entry
    }

    #[allow(dead_code)]
    fn for_each_indexed(&self, mut callback: impl FnMut(u32, &T)) {
        for (page_index, page) in self.pages.iter().enumerate() {
            let Some(page) = page else {
                continue;
            };
            for (entry_index, entry) in page.entries.iter().copied().enumerate() {
                if !entry.is_null() {
                    // SAFETY: Every non-null page entry comes from this
                    // store's arena and stays alive until the store is
                    // dropped.
                    unsafe {
                        callback((page_index * PAGE_SIZE + entry_index) as u32, &*entry);
                    }
                }
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) fn for_each(&self, mut callback: impl FnMut(&T)) {
        self.for_each_indexed(|_, value| callback(value));
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiStaticPositionAlignment {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FfiStaticPositionRect {
    pub rect: LogicalRect,
    pub inline_alignment: FfiStaticPositionAlignment,
    pub block_alignment: FfiStaticPositionAlignment,
    pub alignment_derives_from_own_computed_values: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
#[allow(dead_code)]
pub enum FfiAbsposAxisMode {
    StaticPosition,
    InsetFromRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiAbsposAlignment {
    Baseline,
    Center,
    End,
    Normal,
    Safe,
    SelfEnd,
    SelfStart,
    SpaceAround,
    SpaceBetween,
    SpaceEvenly,
    Start,
    Stretch,
    Unsafe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FfiAbsposContainingBlockInfo {
    pub rect: LogicalRect,
    pub inline_axis_mode: FfiAbsposAxisMode,
    pub block_axis_mode: FfiAbsposAxisMode,
    pub has_inline_alignment: bool,
    pub inline_alignment: FfiAbsposAlignment,
    pub has_block_alignment: bool,
    pub block_alignment: FfiAbsposAlignment,
    pub derives_from_own_computed_values: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FfiAbsposLayoutInputs {
    pub static_position_rect: FfiStaticPositionRect,
    pub containing_block_info: FfiAbsposContainingBlockInfo,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiTableCellCoordinates {
    pub row_index: usize,
    pub column_index: usize,
    pub row_span: usize,
    pub column_span: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiCommittedBoxMetrics {
    pub content_inline_size: crate::css_pixels::CssPixels,
    pub content_block_size: crate::css_pixels::CssPixels,
    pub margin_left: crate::css_pixels::CssPixels,
    pub margin_right: crate::css_pixels::CssPixels,
    pub margin_top: crate::css_pixels::CssPixels,
    pub margin_bottom: crate::css_pixels::CssPixels,
    pub border_left: crate::css_pixels::CssPixels,
    pub border_right: crate::css_pixels::CssPixels,
    pub border_top: crate::css_pixels::CssPixels,
    pub border_bottom: crate::css_pixels::CssPixels,
    pub padding_left: crate::css_pixels::CssPixels,
    pub padding_right: crate::css_pixels::CssPixels,
    pub padding_top: crate::css_pixels::CssPixels,
    pub padding_bottom: crate::css_pixels::CssPixels,
    pub inset_left: crate::css_pixels::CssPixels,
    pub inset_right: crate::css_pixels::CssPixels,
    pub inset_top: crate::css_pixels::CssPixels,
    pub inset_bottom: crate::css_pixels::CssPixels,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiCommittedOffset {
    pub offset: crate::used_values::FfiCssPixelPoint,
    pub inset_left: crate::css_pixels::CssPixels,
    pub inset_top: crate::css_pixels::CssPixels,
    pub materialized_from_paintable: bool,
    pub has_containing_line_box_fragment: bool,
    pub containing_line_box_index: usize,
    pub line_fragment_lookup: u8,
    pub line_fragment_offset: crate::used_values::FfiCssPixelPoint,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiCommitNodeResult {
    pub paintable: *mut c_void,
    pub paintable_for_children: *mut c_void,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiCommitPosition {
    pub parent_paintable: *mut c_void,
    pub insert_before_paintable: *mut c_void,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiPaintableGeometry {
    pub content_inline_size: crate::css_pixels::CssPixels,
    pub content_block_size: crate::css_pixels::CssPixels,
    pub content_offset: crate::used_values::FfiCssPixelPoint,
    pub margin_left: crate::css_pixels::CssPixels,
    pub margin_right: crate::css_pixels::CssPixels,
    pub margin_top: crate::css_pixels::CssPixels,
    pub margin_bottom: crate::css_pixels::CssPixels,
    pub border_left: crate::css_pixels::CssPixels,
    pub border_right: crate::css_pixels::CssPixels,
    pub border_top: crate::css_pixels::CssPixels,
    pub border_bottom: crate::css_pixels::CssPixels,
    pub padding_left: crate::css_pixels::CssPixels,
    pub padding_right: crate::css_pixels::CssPixels,
    pub padding_top: crate::css_pixels::CssPixels,
    pub padding_bottom: crate::css_pixels::CssPixels,
    pub inset_left: crate::css_pixels::CssPixels,
    pub inset_right: crate::css_pixels::CssPixels,
    pub inset_top: crate::css_pixels::CssPixels,
    pub inset_bottom: crate::css_pixels::CssPixels,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiCommitSink {
    pub context: *mut c_void,
    pub begin_commit: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> FfiCommitPosition,
    pub finish_commit: unsafe extern "C" fn(*mut c_void),
    pub prepare_node: unsafe extern "C" fn(*mut c_void, *mut c_void, bool, bool, FfiAbsposLayoutInputs) -> *mut c_void,
    pub set_box_metrics: unsafe extern "C" fn(*mut c_void, *mut c_void, FfiCommittedBoxMetrics),
    pub set_override_borders: unsafe extern "C" fn(*mut c_void, *mut c_void, FfiBordersData),
    pub set_table_cell_coordinates: unsafe extern "C" fn(*mut c_void, *mut c_void, FfiTableCellCoordinates),
    pub begin_line_data: unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool,
    pub begin_line: unsafe extern "C" fn(*mut c_void, FfiLineRecord),
    pub emit_fragment: unsafe extern "C" fn(*mut c_void, FfiCommittedFragment),
    pub emit_inline_box_piece: unsafe extern "C" fn(*mut c_void, FfiInlineBoxPiece),
    pub finish_line_data: unsafe extern "C" fn(*mut c_void),
    pub set_computed_svg_transforms:
        unsafe extern "C" fn(*mut c_void, *mut c_void, crate::formatting_context::svg::FfiSvgComputedTransforms),
    pub set_computed_svg_path: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
    pub set_grid_layout_data: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
    pub set_flex_layout_data: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
    pub set_used_grid_tracks: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
    pub set_offset: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, FfiCommittedOffset),
    pub finish_node:
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, *mut c_void) -> FfiCommitNodeResult,
    pub resolve_relative_positions: unsafe extern "C" fn(*mut c_void, *mut c_void),
}

pub(crate) type ReleaseRetainedLayoutHandle = unsafe extern "C" fn(*mut c_void, *mut c_void);

pub(crate) struct RetainedLayoutHandle {
    handle: *mut c_void,
    callback_context: *mut c_void,
    release: ReleaseRetainedLayoutHandle,
}

impl RetainedLayoutHandle {
    pub(crate) fn new(
        handle: *mut c_void,
        callback_context: *mut c_void,
        release: ReleaseRetainedLayoutHandle,
    ) -> Self {
        assert!(!handle.is_null());
        Self {
            handle,
            callback_context,
            release,
        }
    }

    pub(crate) fn take(&mut self) -> *mut c_void {
        let handle = self.handle;
        self.handle = null_mut();
        handle
    }
}

impl Drop for RetainedLayoutHandle {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        // SAFETY: Each retained layout handle is returned with one ownership
        // unit by its creating callback and is either transferred to the
        // commit sink or released exactly once with the paired callback.
        unsafe {
            (self.release)(self.callback_context, self.handle);
        }
    }
}

#[derive(Clone, Copy)]
struct ContainedAbsposChild {
    child_box: *mut c_void,
    child_layout_index: u32,
    static_position_rect: FfiStaticPositionRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct FfiContainedAbsposChild {
    pub child_box: *mut c_void,
    pub static_position_rect: FfiStaticPositionRect,
}

#[allow(dead_code)]
pub(crate) struct LayoutState {
    used_values: PagedStore<UsedValuesCore>,
    contained_abspos_children: HashMap<usize, Vec<ContainedAbsposChild>>,
    lazy_style_values: PagedStore<LazyStyleCache>,
    box_facts: PagedStore<FfiLayoutBoxFacts>,
    table_facts: PagedStore<FfiTableBoxFacts>,
    grid_facts: PagedStore<GridStyleFacts>,
    text_facts: HashMap<usize, TextNodeFacts>,
    line_data: PagedStore<LineData>,
    block_rare_data: PagedStore<BlockRareData>,
    used_values_rare_data: PagedStore<UsedValuesRareData>,
    layout_indices_by_node: HashMap<usize, u32>,
    bfc_root_fact_builds_excluded: HashSet<u32>,
    purpose: LayoutStatePurpose,
    may_reuse_precreated_used_values: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LayoutStatePurpose {
    Commit,
    Measurement,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self::new(LayoutStatePurpose::Commit)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BlockRareData {
    pub(crate) lowest_floating_descendant_bottom_margin_edge: Option<crate::css_pixels::CssPixels>,
}

#[derive(Default)]
#[allow(dead_code)]
pub(crate) struct LineData {
    pub(crate) line_boxes: Vec<LineBoxData>,
    pub(crate) inline_box_pieces: Vec<InlineBoxPieceData>,
}

#[derive(Default)]
pub(crate) struct UsedValuesRareData {
    pub(crate) table_cell_coordinates: Option<FfiTableCellCoordinates>,
    pub(crate) computed_svg_path: Option<RetainedLayoutHandle>,
    pub(crate) computed_svg_transforms: Option<crate::formatting_context::svg::FfiSvgComputedTransforms>,
    pub(crate) grid_layout_data: Option<RetainedLayoutHandle>,
    pub(crate) flex_layout_data: Option<RetainedLayoutHandle>,
    pub(crate) used_grid_tracks: Option<RetainedLayoutHandle>,
    pub(crate) override_borders_data: Option<FfiBordersData>,
    pub(crate) abspos_layout_inputs: Option<FfiAbsposLayoutInputs>,
}

#[allow(dead_code)]
impl LayoutState {
    pub(crate) fn new(purpose: LayoutStatePurpose) -> Self {
        Self {
            used_values: PagedStore::default(),
            contained_abspos_children: HashMap::new(),
            lazy_style_values: PagedStore::default(),
            box_facts: PagedStore::default(),
            table_facts: PagedStore::default(),
            grid_facts: PagedStore::default(),
            text_facts: HashMap::new(),
            line_data: PagedStore::default(),
            block_rare_data: PagedStore::default(),
            used_values_rare_data: PagedStore::default(),
            layout_indices_by_node: HashMap::new(),
            bfc_root_fact_builds_excluded: HashSet::new(),
            purpose,
            may_reuse_precreated_used_values: false,
        }
    }

    pub(crate) fn is_measurement(&self) -> bool {
        self.purpose == LayoutStatePurpose::Measurement
    }

    pub(crate) fn allow_precreated_used_values_reuse(&mut self) {
        assert!(!self.is_measurement());
        self.may_reuse_precreated_used_values = true;
    }

    pub(crate) fn ensure_capacity(&mut self, count: u32) {
        self.used_values.ensure_capacity(count);
    }

    fn layout_index(&mut self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> u32 {
        if let Some(index) = self.layout_indices_by_node.get(&(node as usize)) {
            return *index;
        }
        let facts = self.build_box_facts(callbacks, node);
        assert!(facts.has_layout_index);
        facts.layout_index
    }

    fn build_box_facts(&mut self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> FfiLayoutBoxFacts {
        bump(FfiOp::NavigationCallback);
        // SAFETY: The callback table and node are supplied by the live C++
        // formatting-context shim and remain valid for this layout pass.
        let facts = unsafe { (callbacks.build_box_facts)(callbacks.context, node) };
        if !facts.has_layout_index {
            return facts;
        }
        let index = facts.layout_index;
        self.layout_indices_by_node.insert(node as usize, index);
        let existing = self.box_facts.get(index);
        if existing.is_null() {
            self.box_facts.allocate(index, facts);
            facts
        } else {
            bump(FfiOp::BoxFactsCacheHit);
            // SAFETY: Non-null store entries remain valid for the state.
            unsafe { *existing }
        }
    }

    pub(crate) fn box_facts(&mut self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> FfiLayoutBoxFacts {
        if let Some(index) = self.layout_indices_by_node.get(&(node as usize)) {
            let facts = self.box_facts.get(*index);
            if !facts.is_null() {
                bump(FfiOp::BoxFactsCacheHit);
                // SAFETY: Non-null store entries remain valid for the state.
                let mut facts = unsafe { *facts };
                facts.may_reuse_precreated_used_values =
                    self.may_reuse_precreated_used_values && !self.used_values.get(*index).is_null();
                return facts;
            }
        }
        let mut facts = self.build_box_facts(callbacks, node);
        facts.may_reuse_precreated_used_values = facts.has_layout_index
            && self.may_reuse_precreated_used_values
            && !self.used_values.get(facts.layout_index).is_null();
        facts
    }

    pub(crate) fn create_used_values(
        &mut self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
        constraints: FfiContainingBlockConstraints,
    ) -> *mut UsedValuesCore {
        assert!(!node.is_null());
        let facts = self.box_facts(callbacks, node);
        assert!(facts.has_layout_index);
        assert!(self.used_values.get(facts.layout_index).is_null());

        let style = self.style_facts(callbacks, node);
        let percentage_basis_inline_size = constraints
            .has_percentage_basis_inline_size
            .then_some(constraints.percentage_basis_inline_size);
        let percentage_basis_block_size = constraints
            .has_percentage_basis_block_size
            .then_some(constraints.percentage_basis_block_size);

        // NOTE: In the code below, we decide if `node` has definite inline
        // and/or block size. This attempts to cover all the *general* cases
        // where CSS considers sizes to be definite. If `node` has definite
        // values for min/max-width or min/max-height and a definite preferred
        // size in the same axis, we clamp the preferred size here as well.
        //
        // There are additional cases where CSS considers values to be
        // definite. We model all of those by considering sizes definite once
        // they are assigned through set_content_inline_size() or
        // set_content_block_size().
        let mut used = UsedValuesCore {
            node,
            ..UsedValuesCore::default()
        };

        #[derive(Clone, Copy)]
        enum Axis {
            Inline,
            Block,
        }

        let containing_block_size_for_axis = |axis: Axis| match axis {
            Axis::Inline => percentage_basis_inline_size.unwrap_or_default(),
            Axis::Block => percentage_basis_block_size.unwrap_or_default(),
        };
        let containing_block_has_definite_size = |axis: Axis| match axis {
            Axis::Inline => percentage_basis_inline_size.is_some(),
            Axis::Block => percentage_basis_block_size.is_some(),
        };

        let adjust_for_box_sizing =
            |unadjusted: crate::css_pixels::CssPixels, computed_size: crate::style_facts::FfiSizeValue, axis: Axis| {
                const BOX_SIZING_CONTENT_BOX: u8 = 1;
                // box-sizing: content-box and automatic sizes need no
                // adjustment.
                if style.box_sizing == BOX_SIZING_CONTENT_BOX || computed_size.is_auto() {
                    return unadjusted;
                }

                // box-sizing: border-box subtracts the relevant border and
                // padding. Block-axis padding percentages also resolve against
                // the containing block's inline size.
                let inline_basis = percentage_basis_inline_size.unwrap_or_default();
                let border_and_padding = match axis {
                    Axis::Inline => {
                        style.border_left_width
                            + style.padding_left().to_px(inline_basis)
                            + style.border_right_width
                            + style.padding_right().to_px(inline_basis)
                    }
                    Axis::Block => {
                        style.border_top_width
                            + style.padding_top().to_px(inline_basis)
                            + style.border_bottom_width
                            + style.padding_bottom().to_px(inline_basis)
                    }
                };
                unadjusted - border_and_padding
            };

        let parent = unsafe { (callbacks.navigation.parent)(callbacks.navigation.context, node) };
        let parent_facts = (!parent.is_null()).then(|| self.box_facts(callbacks, parent));
        let is_definite_size = |size: crate::style_facts::FfiSizeValue,
                                axis: Axis|
         -> Option<crate::css_pixels::CssPixels> {
            // A definite size can be determined without performing
            // layout: a length, an initial-containing-block size, or a
            // percentage/formula resolved solely against definite sizes.
            if size.is_auto() {
                // The inline size of a non-flex-item block is definite when
                // it is auto and its containing block has a definite inline
                // size. This is the stretch-fit case from css-sizing-3.
                // Replaced boxes remain content-based until layout.
                if matches!(axis, Axis::Inline)
                    && !facts.is_replaced_box
                    && !facts.is_floating
                    && !facts.is_absolutely_positioned
                    && facts.display.is_block_outside()
                    && parent_facts.is_some_and(|parent| {
                        !parent.is_floating && (parent.display.is_flow_root_inside() || parent.display.is_flow_inside())
                    })
                    && containing_block_has_definite_size(Axis::Inline)
                {
                    let available = containing_block_size_for_axis(Axis::Inline);
                    return Some(UsedValuesCore::clamp_dimension(
                        available
                            - used.margin_left
                            - used.margin_right
                            - used.padding_left
                            - used.padding_right
                            - used.border_left
                            - used.border_right,
                    ));
                }
                return None;
            }

            if !size.is_length_percentage() {
                return None;
            }
            if size.contains_percentage && !containing_block_has_definite_size(axis) {
                return None;
            }
            let basis = if size.contains_percentage {
                containing_block_size_for_axis(axis)
            } else {
                crate::css_pixels::CssPixels::default()
            };
            Some(UsedValuesCore::clamp_dimension(adjust_for_box_sizing(
                size.to_px(basis),
                size,
                axis,
            )))
        };

        let min_inline_size = is_definite_size(style.min_width(), Axis::Inline);
        let max_inline_size = is_definite_size(style.max_width(), Axis::Inline);
        let min_block_size = is_definite_size(style.min_height(), Axis::Block);
        let max_block_size = is_definite_size(style.max_height(), Axis::Block);
        let mut content_inline_size = is_definite_size(style.width(), Axis::Inline);
        let mut content_block_size = is_definite_size(style.height(), Axis::Block);

        used.has_definite_inline_size = content_inline_size.is_some();
        used.has_definite_block_size = content_block_size.is_some();
        if let Some(size) = content_inline_size.as_mut() {
            if let Some(minimum) = min_inline_size {
                *size = UsedValuesCore::clamp_dimension((*size).max(minimum));
            }
            if let Some(maximum) = max_inline_size {
                *size = UsedValuesCore::clamp_dimension((*size).min(maximum));
            }
        }
        if let Some(size) = content_block_size.as_mut() {
            if let Some(minimum) = min_block_size {
                *size = UsedValuesCore::clamp_dimension((*size).max(minimum));
            }
            if let Some(maximum) = max_block_size {
                *size = UsedValuesCore::clamp_dimension((*size).min(maximum));
            }
        }
        used.content_inline_size = content_inline_size.unwrap_or_default();
        used.content_block_size = content_block_size.unwrap_or_default();

        bump(FfiOp::UsedValuesCreate);
        let used = self.used_values.allocate(facts.layout_index, used);

        if facts.is_list_item_box && !facts.list_item_marker.is_null() {
            let marker_facts = self.box_facts(callbacks, facts.list_item_marker);
            assert!(marker_facts.has_layout_index);
            if self.used_values.get(marker_facts.layout_index).is_null() {
                // List markers inherit only bases that were already definite
                // when their list item entry was created.
                let used_ref = unsafe { &*used };
                let marker_constraints = FfiContainingBlockConstraints {
                    has_percentage_basis_inline_size: used_ref.has_definite_inline_size(),
                    percentage_basis_inline_size: used_ref.content_inline_size,
                    has_percentage_basis_block_size: used_ref.has_definite_block_size(),
                    percentage_basis_block_size: used_ref.content_block_size,
                    ..FfiContainingBlockConstraints::default()
                };
                self.create_used_values(callbacks, facts.list_item_marker, marker_constraints);
            }
        }

        used
    }

    pub(crate) fn populate_from_paintable(
        &mut self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
        paintable: *mut c_void,
    ) -> Option<*mut UsedValuesCore> {
        let facts = self.box_facts(callbacks, node);
        assert!(facts.has_layout_index);
        assert!(self.used_values.get(facts.layout_index).is_null());

        let mut geometry = FfiPaintableGeometry::default();
        let found =
            unsafe { (callbacks.read_paintable_geometry)(callbacks.context, node, paintable, &raw mut geometry) };
        if !found {
            return None;
        }

        // Skip normal node initialization: resolving computed sizes requires
        // percentage bases, and every resulting geometry field is replaced by
        // the previous paintable's committed value immediately.
        let mut used = UsedValuesCore {
            node,
            materialized_from_paintable: true,
            ..UsedValuesCore::default()
        };
        used.set_content_inline_size(geometry.content_inline_size);
        used.set_content_block_size(geometry.content_block_size);
        used.has_definite_inline_size = true;
        used.has_definite_block_size = true;
        used.has_content_offset = true;
        used.content_offset = geometry.content_offset;
        used.margin_left = geometry.margin_left;
        used.margin_right = geometry.margin_right;
        used.margin_top = geometry.margin_top;
        used.margin_bottom = geometry.margin_bottom;
        used.border_left = geometry.border_left;
        used.border_right = geometry.border_right;
        used.border_top = geometry.border_top;
        used.border_bottom = geometry.border_bottom;
        used.padding_left = geometry.padding_left;
        used.padding_right = geometry.padding_right;
        used.padding_top = geometry.padding_top;
        used.padding_bottom = geometry.padding_bottom;
        used.inset_left = geometry.inset_left;
        used.inset_right = geometry.inset_right;
        used.inset_top = geometry.inset_top;
        used.inset_bottom = geometry.inset_bottom;

        bump(FfiOp::UsedValuesCreate);
        Some(self.used_values.allocate(facts.layout_index, used))
    }

    pub(crate) fn used_value_nodes(&self) -> Vec<*mut c_void> {
        let mut nodes = Vec::new();
        self.used_values.for_each(|used| nodes.push(used.node));
        nodes
    }

    pub(crate) fn set_box_is_grid_item(
        &mut self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
        is_grid_item: bool,
    ) {
        let index = self.layout_index(callbacks, node);
        let facts = self.box_facts.get(index);
        assert!(!facts.is_null());
        // SAFETY: The cached facts entry is uniquely owned by this layout state
        // and remains stable for its lifetime.
        unsafe {
            (*facts).is_grid_item = is_grid_item;
            (*facts).vertical_align_applies = !(*facts).is_flex_item && !is_grid_item;
        }
    }

    pub(crate) fn set_box_is_flex_item(
        &mut self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
        is_flex_item: bool,
    ) {
        let index = self.layout_index(callbacks, node);
        let facts = self.box_facts.get(index);
        assert!(!facts.is_null());
        // SAFETY: The cached facts entry is uniquely owned by this layout state
        // and remains stable for its lifetime.
        unsafe {
            (*facts).is_flex_item = is_flex_item;
            (*facts).vertical_align_applies = !is_flex_item && !(*facts).is_grid_item;
        }
    }

    pub(crate) fn mark_bfc_root_fact_builds_excluded(&mut self, layout_index: u32) -> bool {
        self.bfc_root_fact_builds_excluded.insert(layout_index)
    }

    pub(crate) fn style_facts(&mut self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> StyleValues {
        let facts = self.box_facts(callbacks, node);
        assert!(facts.has_layout_index);
        let index = facts.layout_index;
        let mut cache = self.lazy_style_values.get(index);
        if cache.is_null() {
            bump(FfiOp::StyleViewCreate);
            cache = self.lazy_style_values.allocate(index, LazyStyleCache::new());
        } else {
            bump(FfiOp::StyleViewCacheHit);
        }
        StyleValues::new(
            facts.style_payloads,
            facts.display,
            cache,
            callbacks.context,
            callbacks.decode_style_field,
        )
    }

    pub(crate) fn replace_resolved_anchor_insets(
        &mut self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
        resolved: crate::formatting_context::abspos::FfiResolvedAnchorInsets,
    ) {
        let index = self.layout_index(callbacks, node);
        let cache = self.lazy_style_values.get(index);
        assert!(!cache.is_null());
        // SAFETY: The lazy-style entry is stable and layout mutates it
        // serially when anchor functions become plain used-value insets.
        let cache = unsafe { &mut *cache };
        let mut replace = |field: FfiStyleField, is_auto: bool, value: crate::css_pixels::CssPixels| {
            let value = if is_auto {
                crate::style_facts::FfiSizeValue::auto_value()
            } else {
                crate::style_facts::FfiSizeValue::px_value(value)
            };
            cache.replace_size(field, value);
        };
        if resolved.resolves_top {
            replace(FfiStyleField::InsetTop, resolved.top_is_auto, resolved.top);
        }
        if resolved.resolves_right {
            replace(FfiStyleField::InsetRight, resolved.right_is_auto, resolved.right);
        }
        if resolved.resolves_bottom {
            replace(FfiStyleField::InsetBottom, resolved.bottom_is_auto, resolved.bottom);
        }
        if resolved.resolves_left {
            replace(FfiStyleField::InsetLeft, resolved.left_is_auto, resolved.left);
        }
    }

    pub(crate) fn table_facts(&mut self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> FfiTableBoxFacts {
        let index = self.layout_index(callbacks, node);
        let facts = self.table_facts.get(index);
        if !facts.is_null() {
            bump(FfiOp::TableFactsCacheHit);
            // SAFETY: Non-null store entries remain valid for the state.
            return unsafe { *facts };
        }
        // SAFETY: See build_box_facts().
        let facts = unsafe { (callbacks.build_table_box_facts)(callbacks.context, node) };
        self.table_facts.allocate(index, facts);
        facts
    }

    pub(crate) fn grid_facts(&mut self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> &GridStyleFacts {
        let index = self.layout_index(callbacks, node);
        let facts = self.grid_facts.get(index);
        if !facts.is_null() {
            bump(FfiOp::GridFactsCacheHit);
            // SAFETY: Non-null store entries remain valid for the state.
            return unsafe { &*facts };
        }
        let facts = GridStyleFacts::build(callbacks, node);
        let facts = self.grid_facts.allocate(index, facts);
        // SAFETY: `allocate` returns a live entry owned by the state arena.
        unsafe { &*facts }
    }

    pub(crate) fn text_facts(
        &mut self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
        should_wrap_lines: bool,
        should_respect_linebreaks: bool,
        unidirectional_ltr: bool,
    ) -> &TextNodeFacts {
        let key = node as usize;
        self.text_facts.entry(key).or_insert_with(|| {
            TextNodeFacts::build(
                callbacks,
                node,
                should_wrap_lines,
                should_respect_linebreaks,
                unidirectional_ltr,
            )
        });
        self.text_facts.get(&key).unwrap()
    }

    pub(crate) fn line_data_mut(&mut self, layout_index: u32) -> &mut LineData {
        let data = self.line_data.get(layout_index);
        let data = if data.is_null() {
            self.line_data.allocate(layout_index, LineData::default())
        } else {
            data
        };
        // SAFETY: The entry is uniquely accessed through this mutable state.
        unsafe { &mut *data }
    }

    pub(crate) fn line_data(&self, layout_index: u32) -> Option<&LineData> {
        let data = self.line_data.get(layout_index);
        if data.is_null() {
            None
        } else {
            // SAFETY: Non-null store entries remain valid for the state.
            Some(unsafe { &*data })
        }
    }

    pub(crate) fn line_data_mut_if_present(&mut self, layout_index: u32) -> Option<&mut LineData> {
        let data = self.line_data.get(layout_index);
        if data.is_null() {
            None
        } else {
            // SAFETY: The entry is uniquely accessed through this state.
            Some(unsafe { &mut *data })
        }
    }

    pub(crate) fn block_rare_data(&self, layout_index: u32) -> Option<&BlockRareData> {
        let data = self.block_rare_data.get(layout_index);
        if data.is_null() {
            None
        } else {
            // SAFETY: Non-null store entries remain valid for the state.
            Some(unsafe { &*data })
        }
    }

    pub(crate) fn block_rare_data_mut(&mut self, layout_index: u32) -> &mut BlockRareData {
        let data = self.block_rare_data.get(layout_index);
        let data = if data.is_null() {
            self.block_rare_data.allocate(layout_index, BlockRareData::default())
        } else {
            data
        };
        // SAFETY: The entry is uniquely accessed through this mutable state.
        unsafe { &mut *data }
    }

    pub(crate) fn used_values_rare_data(&self, layout_index: u32) -> Option<&UsedValuesRareData> {
        let data = self.used_values_rare_data.get(layout_index);
        if data.is_null() {
            None
        } else {
            // SAFETY: Non-null store entries remain valid for the state.
            Some(unsafe { &*data })
        }
    }

    pub(crate) fn used_values_rare_data_mut(&mut self, layout_index: u32) -> &mut UsedValuesRareData {
        let data = self.used_values_rare_data.get(layout_index);
        let data = if data.is_null() {
            self.used_values_rare_data
                .allocate(layout_index, UsedValuesRareData::default())
        } else {
            data
        };
        // SAFETY: The entry is uniquely accessed through this mutable state.
        unsafe { &mut *data }
    }

    pub(crate) fn used_values_rare_data_for_node_mut(
        &mut self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
    ) -> &mut UsedValuesRareData {
        let index = self.layout_index(callbacks, node);
        self.used_values_rare_data_mut(index)
    }

    pub(crate) fn used_values_by_index(&self, layout_index: u32) -> Option<&UsedValuesCore> {
        let used = self.used_values.get(layout_index);
        if used.is_null() {
            None
        } else {
            // SAFETY: The state owns this stable entry.
            Some(unsafe { &*used })
        }
    }

    pub(crate) fn used_values(&mut self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> *mut UsedValuesCore {
        let index = self.layout_index(callbacks, node);
        let used_values = self.used_values.get(index);
        assert!(!used_values.is_null());
        used_values
    }

    pub(crate) fn try_used_values(
        &mut self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
    ) -> *mut UsedValuesCore {
        let index = self.layout_index(callbacks, node);
        self.used_values.get(index)
    }

    pub(crate) fn register_contained_abspos_child(
        &mut self,
        target_box: *mut c_void,
        child_box: *mut c_void,
        child_layout_index: u32,
        static_position_rect: FfiStaticPositionRect,
    ) {
        let children = self.contained_abspos_children.entry(target_box as usize).or_default();
        assert!(children.iter().all(|entry| entry.child_box != child_box));
        let insertion_index = children
            .iter()
            .position(|entry| child_layout_index < entry.child_layout_index)
            .unwrap_or(children.len());
        children.insert(
            insertion_index,
            ContainedAbsposChild {
                child_box,
                child_layout_index,
                static_position_rect,
            },
        );
    }

    pub(crate) fn take_next_contained_abspos_child(
        &mut self,
        target_box: *mut c_void,
    ) -> Option<FfiContainedAbsposChild> {
        let key = target_box as usize;
        let children = self.contained_abspos_children.get_mut(&key)?;
        let child = children.remove(0);
        if children.is_empty() {
            self.contained_abspos_children.remove(&key);
        }
        Some(FfiContainedAbsposChild {
            child_box: child.child_box,
            static_position_rect: child.static_position_rect,
        })
    }

    fn used_values_indices_by_node(&self) -> HashMap<usize, u32> {
        let mut indices = HashMap::new();
        self.used_values.for_each_indexed(|index, used| {
            assert!(!used.node.is_null());
            indices.insert(used.node as usize, index);
        });
        indices
    }

    fn line_fragment_lookup(
        &self,
        indices: &HashMap<usize, u32>,
        callbacks: &FfiLayoutFcCallbacks,
        used: &UsedValuesCore,
    ) -> (u8, crate::used_values::FfiCssPixelPoint) {
        if !used.has_containing_line_box_fragment {
            return (0, crate::used_values::FfiCssPixelPoint::default());
        }
        // SAFETY: Commit traverses the still-live C++ layout tree
        // synchronously with the pass callback table.
        let containing_block =
            unsafe { (callbacks.navigation.containing_block)(callbacks.navigation.context, used.node) };
        assert!(!containing_block.is_null());
        let Some(layout_index) = indices.get(&(containing_block as usize)) else {
            return (0, crate::used_values::FfiCssPixelPoint::default());
        };
        let Some(line) = self
            .line_data(*layout_index)
            .and_then(|data| data.line_boxes.get(used.containing_line_box_fragment.line_box_index))
        else {
            return (0, crate::used_values::FfiCssPixelPoint::default());
        };
        let Some(fragment) = line.fragments.get(used.containing_line_box_fragment.fragment_index) else {
            return (1, crate::used_values::FfiCssPixelPoint::default());
        };
        let (x, y) = fragment.offset();
        (2, crate::used_values::FfiCssPixelPoint { x, y })
    }

    fn commit_subtree(
        &mut self,
        node: *mut c_void,
        parent_paintable: *mut c_void,
        insert_before_paintable: *mut c_void,
        indices: &HashMap<usize, u32>,
        callbacks: &FfiLayoutFcCallbacks,
        sink: &FfiCommitSink,
    ) {
        let layout_index = indices.get(&(node as usize)).copied();
        let used = layout_index.and_then(|index| self.used_values_by_index(index));
        let abspos_layout_inputs = layout_index
            .and_then(|index| self.used_values_rare_data(index))
            .and_then(|rare| rare.abspos_layout_inputs);
        // SAFETY: The C++ sink owns paintables and copies every plain-data
        // input synchronously.
        let paintable = unsafe {
            (sink.prepare_node)(
                sink.context,
                node,
                used.is_some(),
                abspos_layout_inputs.is_some(),
                abspos_layout_inputs.unwrap_or(FfiAbsposLayoutInputs {
                    static_position_rect: FfiStaticPositionRect {
                        rect: LogicalRect::default(),
                        inline_alignment: FfiStaticPositionAlignment::Start,
                        block_alignment: FfiStaticPositionAlignment::Start,
                        alignment_derives_from_own_computed_values: false,
                    },
                    containing_block_info: FfiAbsposContainingBlockInfo {
                        rect: LogicalRect::default(),
                        inline_axis_mode: FfiAbsposAxisMode::StaticPosition,
                        block_axis_mode: FfiAbsposAxisMode::StaticPosition,
                        has_inline_alignment: false,
                        inline_alignment: FfiAbsposAlignment::Normal,
                        has_block_alignment: false,
                        block_alignment: FfiAbsposAlignment::Normal,
                        derives_from_own_computed_values: false,
                    },
                }),
            )
        };

        if let (Some(layout_index), Some(used)) = (layout_index, used)
            && !paintable.is_null()
        {
            // SAFETY: Every callback below copies its plain-data argument or
            // consumes one retained handle synchronously.
            unsafe {
                (sink.set_box_metrics)(
                    sink.context,
                    paintable,
                    FfiCommittedBoxMetrics {
                        content_inline_size: used.content_inline_size,
                        content_block_size: used.content_block_size,
                        margin_left: used.margin_left,
                        margin_right: used.margin_right,
                        margin_top: used.margin_top,
                        margin_bottom: used.margin_bottom,
                        border_left: used.border_left,
                        border_right: used.border_right,
                        border_top: used.border_top,
                        border_bottom: used.border_bottom,
                        padding_left: used.padding_left,
                        padding_right: used.padding_right,
                        padding_top: used.padding_top,
                        padding_bottom: used.padding_bottom,
                        inset_left: used.inset_left,
                        inset_right: used.inset_right,
                        inset_top: used.inset_top,
                        inset_bottom: used.inset_bottom,
                    },
                );
            }

            let (line_fragment_lookup, line_fragment_offset) = self.line_fragment_lookup(indices, callbacks, used);
            let committed_offset = FfiCommittedOffset {
                offset: used.content_offset,
                inset_left: used.inset_left,
                inset_top: used.inset_top,
                materialized_from_paintable: used.materialized_from_paintable,
                has_containing_line_box_fragment: used.has_containing_line_box_fragment,
                containing_line_box_index: used.containing_line_box_fragment.line_box_index,
                line_fragment_lookup,
                line_fragment_offset,
            };

            if let Some(rare) = self.used_values_rare_data_mut_if_present(layout_index) {
                unsafe {
                    if let Some(borders) = rare.override_borders_data {
                        (sink.set_override_borders)(sink.context, paintable, borders);
                    }
                    if let Some(coordinates) = rare.table_cell_coordinates {
                        (sink.set_table_cell_coordinates)(sink.context, paintable, coordinates);
                    }
                }
            }

            let has_line_data = self.line_data(layout_index).is_some();
            if has_line_data {
                // SAFETY: The sink keeps one line accumulator live between
                // begin_line_data() and finish_line_data().
                let accepts_lines = unsafe { (sink.begin_line_data)(sink.context, paintable) };
                if accepts_lines {
                    let line_sink = FfiLineSinkCallbacks {
                        context: sink.context,
                        begin_line: sink.begin_line,
                        emit_fragment: sink.emit_fragment,
                        emit_inline_box_piece: sink.emit_inline_box_piece,
                    };
                    assert!(push_line_data(self, layout_index, line_sink));
                    unsafe {
                        (sink.finish_line_data)(sink.context);
                    }
                }
            }

            if let Some(rare) = self.used_values_rare_data_mut_if_present(layout_index) {
                unsafe {
                    if let Some(transforms) = rare.computed_svg_transforms {
                        (sink.set_computed_svg_transforms)(sink.context, paintable, transforms);
                    }
                    if let Some(path) = rare.computed_svg_path.as_mut() {
                        (sink.set_computed_svg_path)(sink.context, paintable, path.take());
                    }
                    if let Some(data) = rare.grid_layout_data.as_mut() {
                        (sink.set_grid_layout_data)(sink.context, paintable, data.take());
                    }
                    if let Some(data) = rare.flex_layout_data.as_mut() {
                        (sink.set_flex_layout_data)(sink.context, paintable, data.take());
                    }
                    if let Some(tracks) = rare.used_grid_tracks.as_mut() {
                        (sink.set_used_grid_tracks)(sink.context, paintable, tracks.take());
                    }
                }
            }
            unsafe {
                (sink.set_offset)(sink.context, node, paintable, committed_offset);
            }
        }

        // SAFETY: Wiring uses only live layout and paintable pointers for this
        // synchronous commit.
        let result =
            unsafe { (sink.finish_node)(sink.context, node, paintable, parent_paintable, insert_before_paintable) };
        assert_eq!(result.paintable, paintable);

        // SAFETY: Navigation callbacks return layout-tree pointers that remain
        // live for this whole commit.
        let mut child = unsafe { (callbacks.navigation.first_child)(callbacks.navigation.context, node) };
        while !child.is_null() {
            let next = unsafe { (callbacks.navigation.next_sibling)(callbacks.navigation.context, child) };
            self.commit_subtree(
                child,
                result.paintable_for_children,
                null_mut(),
                indices,
                callbacks,
                sink,
            );
            child = next;
        }
    }

    pub(crate) fn commit_at(
        &mut self,
        root: *mut c_void,
        parent_paintable: *mut c_void,
        insert_before_paintable: *mut c_void,
        callbacks: &FfiLayoutFcCallbacks,
        sink: &FfiCommitSink,
    ) {
        let indices = self.used_values_indices_by_node();
        self.commit_subtree(
            root,
            parent_paintable,
            insert_before_paintable,
            &indices,
            callbacks,
            sink,
        );

        // Relative inline ancestor offsets require the complete paint tree.
        // Keep the pass paintable-only on the C++ side, but preserve the used
        // values store's original page/index iteration order.
        self.used_values.for_each(|used| unsafe {
            (sink.resolve_relative_positions)(sink.context, used.node);
        });
    }

    pub(crate) fn commit_replacing(
        &mut self,
        root: *mut c_void,
        paintable_to_replace: *mut c_void,
        callbacks: &FfiLayoutFcCallbacks,
        sink: &FfiCommitSink,
    ) {
        // SAFETY: The sink retains the replaced paintable, detaches it, and
        // returns borrowed insertion pointers that stay live until
        // finish_commit().
        let position = unsafe { (sink.begin_commit)(sink.context, root, paintable_to_replace) };
        self.commit_at(
            root,
            position.parent_paintable,
            position.insert_before_paintable,
            callbacks,
            sink,
        );
        unsafe {
            (sink.finish_commit)(sink.context);
        }
    }
}

impl LayoutState {
    fn used_values_rare_data_mut_if_present(&mut self, layout_index: u32) -> Option<&mut UsedValuesRareData> {
        let data = self.used_values_rare_data.get(layout_index);
        if data.is_null() {
            None
        } else {
            // SAFETY: The entry is uniquely accessed through this state.
            Some(unsafe { &mut *data })
        }
    }
}

pub(crate) fn state_mut(state: *mut c_void) -> &'static mut LayoutState {
    assert!(!state.is_null());
    // SAFETY: Every handle points to a Rust LayoutState owned by the active
    // entry or a nested measurement. Formatting contexts borrow it only
    // during that owner's synchronous lifetime.
    unsafe { &mut *state.cast::<LayoutState>() }
}

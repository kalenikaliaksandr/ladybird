/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::box_facts::{BoxFactsView, BoxItemFacts, FfiLayoutBoxFacts, has_flag};
use crate::css_enums::box_sizing;
use crate::css_pixels::clamp_to_max_dimension_value;
use crate::ffi_stats::{FfiOp, bump};
use crate::formatting_context::grid::facts::GridStyleFacts;
use crate::formatting_context::inline::iterator::text::TextNodeFacts;
use crate::formatting_context::inline::{
    FfiCommittedFragment, FfiInlineBoxPiece, FfiLineRecord, FfiLineSinkCallbacks, line_box::LineBoxData,
    pieces::InlineBoxPieceData, push_line_data,
};
use crate::formatting_context::{FfiBordersData, FfiLayoutFcCallbacks, FfiTableBoxFacts};
use crate::geometry::{ContainingBlockConstraints, LogicalRect};
use crate::style_facts::{FfiStyleField, FfiStyleSnapshot, StyleDecodeValues, StyleValues};
use crate::used_values::UsedValuesCore;
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::hash::{BuildHasherDefault, Hasher};
use std::mem::MaybeUninit;
use std::ptr::null_mut;
use std::rc::Rc;

const PAGE_BITS: usize = 4;
const PAGE_SIZE: usize = 1 << PAGE_BITS;
const PAGE_MASK: usize = PAGE_SIZE - 1;
const BUMP_ARENA_CHUNK_SIZE: usize = 4 * 1024;

#[derive(Default)]
struct PointerHasher(u64);

impl Hasher for PointerHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut value = 0u64;
        for &byte in bytes {
            value = value.wrapping_shl(8) | u64::from(byte);
        }
        self.0 = value.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }

    fn write_usize(&mut self, value: usize) {
        self.0 = (value as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

type PointerMap<V> = HashMap<usize, V, BuildHasherDefault<PointerHasher>>;

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

struct Page<T> {
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
    inner: RefCell<PagedStoreInner<T>>,
}

struct PagedStoreInner<T> {
    pages: Vec<Option<Box<Page<T>>>>,
    arena: BumpArena<T>,
}

impl<T> Default for PagedStore<T> {
    fn default() -> Self {
        Self {
            inner: RefCell::new(PagedStoreInner {
                pages: Vec::new(),
                arena: BumpArena::default(),
            }),
        }
    }
}

impl<T> PagedStore<T> {
    pub(crate) fn ensure_capacity(&self, count: u32) {
        let page_count = ((count as usize) + PAGE_SIZE - 1) >> PAGE_BITS;
        self.inner.borrow_mut().pages.resize_with(page_count, || None);
    }

    fn entry_pointer(inner: &PagedStoreInner<T>, index: u32) -> *mut T {
        let index = index as usize;
        let page_index = index >> PAGE_BITS;
        let Some(Some(page)) = inner.pages.get(page_index) else {
            return null_mut();
        };
        page.entries[index & PAGE_MASK]
    }

    #[inline]
    pub(crate) fn get(&self, index: u32) -> Option<&T> {
        let inner = self.inner.borrow();
        let entry = Self::entry_pointer(&inner, index);
        if entry.is_null() {
            return None;
        }
        // SAFETY: Arena entries are Box-stable, never moved or freed before
        // the store, and the returned borrow is tied to the store.
        Some(unsafe { &*entry })
    }

    pub(crate) fn allocate(&self, index: u32, value: T) -> &T {
        let index = index as usize;
        let page_index = index >> PAGE_BITS;
        let mut inner = self.inner.borrow_mut();
        if page_index >= inner.pages.len() {
            inner.pages.resize_with(page_index + 1, || None);
        }
        let entry_index = index & PAGE_MASK;
        assert!(
            inner.pages[page_index]
                .as_ref()
                .is_none_or(|page| page.entries[entry_index].is_null())
        );
        let allocated = inner.arena.allocate(value);
        let page = inner.pages[page_index].get_or_insert_with(|| Box::new(Page::default()));
        page.entries[entry_index] = allocated;
        // SAFETY: The allocation is initialized and remains at this address
        // until the store is dropped.
        unsafe { &*allocated }
    }

    fn for_each_indexed(&self, mut callback: impl FnMut(u32, &T)) {
        let entries = {
            let inner = self.inner.borrow();
            let mut entries = Vec::new();
            for (page_index, page) in inner.pages.iter().enumerate() {
                let Some(page) = page else {
                    continue;
                };
                for (entry_index, entry) in page.entries.iter().copied().enumerate() {
                    if !entry.is_null() {
                        entries.push(((page_index * PAGE_SIZE + entry_index) as u32, entry));
                    }
                }
            }
            entries
        };
        for (index, entry) in entries {
            // SAFETY: Every collected pointer belongs to this append-only
            // arena and stays live for the whole store lifetime.
            callback(index, unsafe { &*entry });
        }
    }

    pub(crate) fn for_each(&self, mut callback: impl FnMut(&T)) {
        self.for_each_indexed(|_, value| callback(value));
    }
}

struct PointerStore<T> {
    inner: RefCell<PointerStoreInner<T>>,
}

struct PointerStoreInner<T> {
    entries: PointerMap<*mut T>,
    arena: BumpArena<T>,
}

impl<T> Default for PointerStore<T> {
    fn default() -> Self {
        Self {
            inner: RefCell::new(PointerStoreInner {
                entries: PointerMap::default(),
                arena: BumpArena::default(),
            }),
        }
    }
}

impl<T> PointerStore<T> {
    #[inline]
    fn get(&self, key: usize) -> Option<&T> {
        let inner = self.inner.borrow();
        let entry = inner.entries.get(&key).copied()?;
        // SAFETY: Entries live in the append-only arena and are never moved
        // or freed before the store, so the reference may outlive this guard.
        Some(unsafe { &*entry })
    }

    fn allocate(&self, key: usize, value: T) -> &T {
        let mut inner = self.inner.borrow_mut();
        assert!(!inner.entries.contains_key(&key));
        let allocated = inner.arena.allocate(value);
        inner.entries.insert(key, allocated);
        // SAFETY: The allocation is initialized and remains at this address
        // until the store is dropped.
        unsafe { &*allocated }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StaticPositionAlignment {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StaticPositionRect {
    pub(crate) rect: LogicalRect,
    pub(crate) inline_alignment: StaticPositionAlignment,
    pub(crate) block_alignment: StaticPositionAlignment,
    pub(crate) alignment_derives_from_own_computed_values: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AbsposAxisMode {
    StaticPosition,
    InsetFromRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AbsposAlignment {
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
pub(crate) struct AbsposContainingBlockInfo {
    pub(crate) rect: LogicalRect,
    pub(crate) inline_axis_mode: AbsposAxisMode,
    pub(crate) block_axis_mode: AbsposAxisMode,
    pub(crate) has_inline_alignment: bool,
    pub(crate) inline_alignment: AbsposAlignment,
    pub(crate) has_block_alignment: bool,
    pub(crate) block_alignment: AbsposAlignment,
    pub(crate) derives_from_own_computed_values: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AbsposLayoutInputs {
    pub(crate) static_position_rect: StaticPositionRect,
    pub(crate) containing_block_info: AbsposContainingBlockInfo,
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
    pub line_fragment_lookup: FfiLineFragmentLookup,
    pub line_fragment_offset: crate::used_values::FfiCssPixelPoint,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiLineFragmentLookup {
    #[default]
    NotFound = 0,
    LineOnly = 1,
    Found = 2,
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
    pub prepare_node: unsafe extern "C" fn(*mut c_void, *mut c_void, bool) -> *mut c_void,
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
    static_position_rect: StaticPositionRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PendingAbsposChild {
    pub(crate) child_box: *mut c_void,
    pub(crate) static_position_rect: StaticPositionRect,
}

struct BoxFactsEntry {
    facts: FfiLayoutBoxFacts,
    item_facts: Cell<BoxItemFacts>,
}

impl BoxFactsEntry {
    fn new(facts: FfiLayoutBoxFacts) -> Self {
        Self {
            item_facts: Cell::new(BoxItemFacts {
                is_flex_item: facts.is_flex_item,
                is_grid_item: facts.is_grid_item,
                vertical_align_applies: facts.vertical_align_applies,
            }),
            facts,
        }
    }

    #[inline]
    fn view(&self) -> BoxFactsView<'_> {
        BoxFactsView::new(&self.facts, self.item_facts.get())
    }
}

pub(crate) struct LayoutState {
    used_values: PagedStore<UsedValuesCore>,
    contained_abspos_children: RefCell<PointerMap<Rc<RefCell<Vec<ContainedAbsposChild>>>>>,
    style_decode_values: PagedStore<StyleDecodeValues>,
    style_snapshots: PagedStore<FfiStyleSnapshot>,
    box_facts: PagedStore<BoxFactsEntry>,
    unindexed_box_facts: PointerStore<BoxFactsEntry>,
    table_facts: PagedStore<FfiTableBoxFacts>,
    grid_facts: PagedStore<GridStyleFacts>,
    text_facts: PointerStore<TextNodeFacts>,
    line_data: PagedStore<RefCell<LineData>>,
    block_rare_data: PagedStore<RefCell<BlockRareData>>,
    used_values_rare_data: PagedStore<RefCell<UsedValuesRareData>>,
    bfc_root_fact_builds_excluded: RefCell<HashSet<u32>>,
    precreated_used_value_indices: RefCell<HashSet<u32>>,
    purpose: LayoutStatePurpose,
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
    pub(crate) abspos_layout_inputs: Option<AbsposLayoutInputs>,
}

impl LayoutState {
    pub(crate) fn new(purpose: LayoutStatePurpose) -> Self {
        Self {
            used_values: PagedStore::default(),
            contained_abspos_children: RefCell::new(PointerMap::default()),
            style_decode_values: PagedStore::default(),
            style_snapshots: PagedStore::default(),
            box_facts: PagedStore::default(),
            unindexed_box_facts: PointerStore::default(),
            table_facts: PagedStore::default(),
            grid_facts: PagedStore::default(),
            text_facts: PointerStore::default(),
            line_data: PagedStore::default(),
            block_rare_data: PagedStore::default(),
            used_values_rare_data: PagedStore::default(),
            bfc_root_fact_builds_excluded: RefCell::new(HashSet::new()),
            precreated_used_value_indices: RefCell::new(HashSet::new()),
            purpose,
        }
    }

    pub(crate) fn is_measurement(&self) -> bool {
        self.purpose == LayoutStatePurpose::Measurement
    }

    pub(crate) fn record_precreated_used_values(&self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) {
        assert!(!self.is_measurement());
        let index = self.layout_index(callbacks, node);
        assert!(self.used_values.get(index).is_some());
        assert!(self.precreated_used_value_indices.borrow_mut().insert(index));
    }

    pub(crate) fn ensure_capacity(&self, count: u32) {
        self.used_values.ensure_capacity(count);
    }

    #[inline]
    fn layout_index(&self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> u32 {
        let data = callbacks.node_data(node);
        assert!(has_flag(data, crate::node_data::NodeFlag::HasStyle));
        data.layout_index
    }

    #[inline]
    fn optional_layout_index(&self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> Option<u32> {
        let data = callbacks.node_data(node);
        has_flag(data, crate::node_data::NodeFlag::HasStyle).then_some(data.layout_index)
    }

    fn build_box_facts<'pass>(
        &'pass self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
    ) -> &'pass BoxFactsEntry {
        bump(FfiOp::NodeQuery);
        // SAFETY: The callback table and node are supplied by the live C++
        // formatting-context shim and remain valid for this layout pass.
        let additional = unsafe { (callbacks.build_box_facts)(callbacks.context, node) };
        let data = callbacks.node_data(node);
        let display = if data.style.is_null() {
            crate::display::FfiDisplay::block()
        } else {
            self.style_snapshot(callbacks, node).display
        };
        let facts = FfiLayoutBoxFacts::from_arena(data, additional, display);
        if facts.has_layout_index {
            let index = facts.layout_index;
            if let Some(existing) = self.box_facts.get(index) {
                bump(FfiOp::BoxFactsCacheHit);
                existing
            } else {
                self.box_facts.allocate(index, BoxFactsEntry::new(facts))
            }
        } else if let Some(existing) = self.unindexed_box_facts.get(node as usize) {
            bump(FfiOp::BoxFactsCacheHit);
            existing
        } else {
            self.unindexed_box_facts
                .allocate(node as usize, BoxFactsEntry::new(facts))
        }
    }

    #[inline]
    pub(crate) fn box_facts<'pass>(
        &'pass self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
    ) -> BoxFactsView<'pass> {
        let data = callbacks.node_data(node);
        let entry = if has_flag(data, crate::node_data::NodeFlag::HasStyle) {
            self.box_facts.get(data.layout_index)
        } else {
            self.unindexed_box_facts.get(node as usize)
        };
        if let Some(entry) = entry {
            bump(FfiOp::BoxFactsCacheHit);
            entry.view()
        } else {
            self.build_box_facts(callbacks, node).view()
        }
    }

    pub(crate) fn may_reuse_precreated_used_values(&self, index: u32) -> bool {
        self.precreated_used_value_indices.borrow().contains(&index)
    }

    pub(crate) fn create_used_values(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
        constraints: ContainingBlockConstraints,
    ) -> &UsedValuesCore {
        assert!(!node.is_null());
        let facts = self.box_facts(callbacks, node);
        assert!(facts.has_layout_index);
        assert!(self.used_values.get(facts.layout_index).is_none());

        let style = self.style_facts(callbacks, node);
        let percentage_basis_inline_size = constraints.percentage_basis_inline_size;
        let percentage_basis_block_size = constraints.percentage_basis_block_size;

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
        let used = UsedValuesCore {
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
                // box-sizing: content-box and automatic sizes need no
                // adjustment.
                if style.box_sizing == box_sizing::CONTENT_BOX || computed_size.is_auto() {
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

        let parent = callbacks.parent(node);
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
                    return Some(clamp_to_max_dimension_value(
                        available
                            - used.margin_left.get()
                            - used.margin_right.get()
                            - used.padding_left.get()
                            - used.padding_right.get()
                            - used.border_left.get()
                            - used.border_right.get(),
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
            Some(clamp_to_max_dimension_value(adjust_for_box_sizing(
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

        used.has_definite_inline_size.set(content_inline_size.is_some());
        used.has_definite_block_size.set(content_block_size.is_some());
        if let Some(size) = content_inline_size.as_mut() {
            if let Some(minimum) = min_inline_size {
                *size = clamp_to_max_dimension_value((*size).max(minimum));
            }
            if let Some(maximum) = max_inline_size {
                *size = clamp_to_max_dimension_value((*size).min(maximum));
            }
        }
        if let Some(size) = content_block_size.as_mut() {
            if let Some(minimum) = min_block_size {
                *size = clamp_to_max_dimension_value((*size).max(minimum));
            }
            if let Some(maximum) = max_block_size {
                *size = clamp_to_max_dimension_value((*size).min(maximum));
            }
        }
        used.content_inline_size.set(content_inline_size.unwrap_or_default());
        used.content_block_size.set(content_block_size.unwrap_or_default());

        bump(FfiOp::UsedValuesCreate);
        let used = self.used_values.allocate(facts.layout_index, used);

        if facts.is_list_item_box && !facts.list_item_marker.is_null() {
            let marker_facts = self.box_facts(callbacks, facts.list_item_marker);
            assert!(marker_facts.has_layout_index);
            if self.used_values.get(marker_facts.layout_index).is_none() {
                // List markers inherit only bases that were already definite
                // when their list item entry was created.
                let marker_constraints = ContainingBlockConstraints {
                    percentage_basis_inline_size: used
                        .has_definite_inline_size()
                        .then_some(used.content_inline_size.get()),
                    percentage_basis_block_size: used
                        .has_definite_block_size()
                        .then_some(used.content_block_size.get()),
                    ..ContainingBlockConstraints::default()
                };
                self.create_used_values(callbacks, facts.list_item_marker, marker_constraints);
            }
        }

        used
    }

    pub(crate) fn populate_from_paintable(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
        paintable: *mut c_void,
    ) -> Option<&UsedValuesCore> {
        let facts = self.box_facts(callbacks, node);
        assert!(facts.has_layout_index);
        assert!(self.used_values.get(facts.layout_index).is_none());

        let mut geometry = FfiPaintableGeometry::default();
        let found =
            unsafe { (callbacks.read_paintable_geometry)(callbacks.context, node, paintable, &raw mut geometry) };
        if !found {
            return None;
        }

        // Skip normal node initialization: resolving computed sizes requires
        // percentage bases, and every resulting geometry field is replaced by
        // the previous paintable's committed value immediately.
        let used = UsedValuesCore {
            node,
            ..UsedValuesCore::default()
        };
        used.materialized_from_paintable.set(true);
        used.set_content_inline_size(geometry.content_inline_size);
        used.set_content_block_size(geometry.content_block_size);
        used.has_definite_inline_size.set(true);
        used.has_definite_block_size.set(true);
        used.has_content_offset.set(true);
        used.content_offset.set(geometry.content_offset);
        used.margin_left.set(geometry.margin_left);
        used.margin_right.set(geometry.margin_right);
        used.margin_top.set(geometry.margin_top);
        used.margin_bottom.set(geometry.margin_bottom);
        used.border_left.set(geometry.border_left);
        used.border_right.set(geometry.border_right);
        used.border_top.set(geometry.border_top);
        used.border_bottom.set(geometry.border_bottom);
        used.padding_left.set(geometry.padding_left);
        used.padding_right.set(geometry.padding_right);
        used.padding_top.set(geometry.padding_top);
        used.padding_bottom.set(geometry.padding_bottom);
        used.inset_left.set(geometry.inset_left);
        used.inset_right.set(geometry.inset_right);
        used.inset_top.set(geometry.inset_top);
        used.inset_bottom.set(geometry.inset_bottom);

        bump(FfiOp::UsedValuesCreate);
        Some(self.used_values.allocate(facts.layout_index, used))
    }

    pub(crate) fn used_value_nodes(&self) -> Vec<*mut c_void> {
        let mut nodes = Vec::new();
        self.used_values.for_each(|used| nodes.push(used.node));
        nodes
    }

    pub(crate) fn set_box_is_grid_item(&self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void, is_grid_item: bool) {
        let index = self.layout_index(callbacks, node);
        let facts = self.box_facts.get(index).expect("missing box facts");
        let mut updated = facts.item_facts.get();
        updated.is_grid_item = is_grid_item;
        updated.vertical_align_applies = !updated.is_flex_item && !is_grid_item;
        facts.item_facts.set(updated);
    }

    pub(crate) fn set_box_is_flex_item(&self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void, is_flex_item: bool) {
        let index = self.layout_index(callbacks, node);
        let facts = self.box_facts.get(index).expect("missing box facts");
        let mut updated = facts.item_facts.get();
        updated.is_flex_item = is_flex_item;
        updated.vertical_align_applies = !is_flex_item && !updated.is_grid_item;
        facts.item_facts.set(updated);
    }

    pub(crate) fn mark_bfc_root_fact_builds_excluded(&self, layout_index: u32) -> bool {
        self.bfc_root_fact_builds_excluded.borrow_mut().insert(layout_index)
    }

    pub(crate) fn style_facts<'pass>(
        &'pass self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
    ) -> StyleValues<'pass> {
        let index = callbacks.node_data(node).layout_index;
        let cache = if let Some(cache) = self.style_decode_values.get(index) {
            bump(FfiOp::StyleViewCacheHit);
            cache
        } else {
            bump(FfiOp::StyleViewCreate);
            let snapshot = self.style_snapshot(callbacks, node);
            self.style_decode_values
                .allocate(index, StyleDecodeValues::new(snapshot.payloads, snapshot.display))
        };
        StyleValues::new(cache, callbacks.context, callbacks.decode_residual_style)
    }

    fn style_snapshot(&self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> &FfiStyleSnapshot {
        let data = callbacks.node_data(node);
        assert!(!data.style.is_null());
        let index = data.layout_index;
        let snapshot = self.style_snapshots.get(index);
        if let Some(snapshot) = snapshot {
            return snapshot;
        }
        // SAFETY: NodeData retains the node's immutable ComputedValues for the
        // synchronous layout pass.
        let snapshot = unsafe { (callbacks.build_style_snapshot)(callbacks.context, data.style) };
        self.style_snapshots.allocate(index, snapshot)
    }

    pub(crate) fn replace_resolved_anchor_insets(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
        resolved: crate::formatting_context::abspos::FfiResolvedAnchorInsets,
    ) {
        let index = self.layout_index(callbacks, node);
        let cache = self.style_decode_values.get(index).expect("missing style decode cache");
        let replace = |field: FfiStyleField, is_auto: bool, value: crate::css_pixels::CssPixels| {
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

    pub(crate) fn table_facts(&self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> FfiTableBoxFacts {
        let index = self.layout_index(callbacks, node);
        let facts = self.table_facts.get(index);
        if let Some(facts) = facts {
            bump(FfiOp::TableFactsCacheHit);
            return *facts;
        }
        // SAFETY: See build_box_facts().
        let facts = unsafe { (callbacks.build_table_box_facts)(callbacks.context, node) };
        self.table_facts.allocate(index, facts);
        facts
    }

    pub(crate) fn grid_facts(&self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> &GridStyleFacts {
        let index = self.layout_index(callbacks, node);
        let facts = self.grid_facts.get(index);
        if let Some(facts) = facts {
            bump(FfiOp::GridFactsCacheHit);
            return facts;
        }
        let facts = GridStyleFacts::build(callbacks, node);
        self.grid_facts.allocate(index, facts)
    }

    pub(crate) fn text_facts(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
        should_wrap_lines: bool,
        should_respect_linebreaks: bool,
        unidirectional_ltr: bool,
    ) -> &TextNodeFacts {
        let key = node as usize;
        if let Some(facts) = self.text_facts.get(key) {
            return facts;
        }
        self.text_facts.allocate(
            key,
            TextNodeFacts::build(
                callbacks,
                node,
                should_wrap_lines,
                should_respect_linebreaks,
                unidirectional_ltr,
            ),
        )
    }

    pub(crate) fn line_data_cell(&self, layout_index: u32) -> &RefCell<LineData> {
        self.line_data
            .get(layout_index)
            .unwrap_or_else(|| self.line_data.allocate(layout_index, RefCell::new(LineData::default())))
    }

    pub(crate) fn line_data(&self, layout_index: u32) -> Option<Ref<'_, LineData>> {
        self.line_data.get(layout_index).map(RefCell::borrow)
    }

    pub(crate) fn line_data_mut_if_present(&self, layout_index: u32) -> Option<RefMut<'_, LineData>> {
        self.line_data.get(layout_index).map(RefCell::borrow_mut)
    }

    pub(crate) fn block_rare_data(&self, layout_index: u32) -> Option<Ref<'_, BlockRareData>> {
        self.block_rare_data.get(layout_index).map(RefCell::borrow)
    }

    pub(crate) fn block_rare_data_mut(&self, layout_index: u32) -> RefMut<'_, BlockRareData> {
        self.block_rare_data
            .get(layout_index)
            .unwrap_or_else(|| {
                self.block_rare_data
                    .allocate(layout_index, RefCell::new(BlockRareData::default()))
            })
            .borrow_mut()
    }

    pub(crate) fn used_values_rare_data(&self, layout_index: u32) -> Option<Ref<'_, UsedValuesRareData>> {
        self.used_values_rare_data.get(layout_index).map(RefCell::borrow)
    }

    pub(crate) fn used_values_rare_data_mut(&self, layout_index: u32) -> RefMut<'_, UsedValuesRareData> {
        self.used_values_rare_data
            .get(layout_index)
            .unwrap_or_else(|| {
                self.used_values_rare_data
                    .allocate(layout_index, RefCell::new(UsedValuesRareData::default()))
            })
            .borrow_mut()
    }

    pub(crate) fn used_values_rare_data_for_node_mut(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
    ) -> RefMut<'_, UsedValuesRareData> {
        let index = self.layout_index(callbacks, node);
        self.used_values_rare_data_mut(index)
    }

    pub(crate) fn used_values_by_index(&self, layout_index: u32) -> Option<&UsedValuesCore> {
        self.used_values.get(layout_index)
    }

    pub(crate) fn used_values(&self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> &UsedValuesCore {
        let index = self.layout_index(callbacks, node);
        self.used_values.get(index).expect("missing used values")
    }

    pub(crate) fn try_used_values(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
    ) -> Option<&UsedValuesCore> {
        let index = self.layout_index(callbacks, node);
        self.used_values.get(index)
    }

    pub(crate) fn register_contained_abspos_child(
        &self,
        target_box: *mut c_void,
        child_box: *mut c_void,
        child_layout_index: u32,
        static_position_rect: StaticPositionRect,
    ) {
        let children = self
            .contained_abspos_children
            .borrow_mut()
            .entry(target_box as usize)
            .or_insert_with(|| Rc::new(RefCell::new(Vec::new())))
            .clone();
        let mut children = children.borrow_mut();
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

    pub(crate) fn take_next_contained_abspos_child(&self, target_box: *mut c_void) -> Option<PendingAbsposChild> {
        let key = target_box as usize;
        let children = self.contained_abspos_children.borrow().get(&key)?.clone();
        let (child, is_empty) = {
            let mut children = children.borrow_mut();
            let child = children.remove(0);
            (child, children.is_empty())
        };
        if is_empty {
            self.contained_abspos_children.borrow_mut().remove(&key);
        }
        Some(PendingAbsposChild {
            child_box: child.child_box,
            static_position_rect: child.static_position_rect,
        })
    }

    fn line_fragment_lookup(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        used: &UsedValuesCore,
    ) -> (FfiLineFragmentLookup, crate::used_values::FfiCssPixelPoint) {
        if !used.has_containing_line_box_fragment.get() {
            return (
                FfiLineFragmentLookup::NotFound,
                crate::used_values::FfiCssPixelPoint::default(),
            );
        }
        // SAFETY: Commit traverses the still-live C++ layout tree
        // synchronously with the pass callback table.
        let containing_block = callbacks.containing_block(used.node);
        assert!(!containing_block.is_null());
        let Some(layout_index) = self.optional_layout_index(callbacks, containing_block) else {
            return (
                FfiLineFragmentLookup::NotFound,
                crate::used_values::FfiCssPixelPoint::default(),
            );
        };
        let Some(data) = self.line_data(layout_index) else {
            return (
                FfiLineFragmentLookup::NotFound,
                crate::used_values::FfiCssPixelPoint::default(),
            );
        };
        let coordinate = used.containing_line_box_fragment.get();
        let Some(line) = data.line_boxes.get(coordinate.line_box_index) else {
            return (
                FfiLineFragmentLookup::NotFound,
                crate::used_values::FfiCssPixelPoint::default(),
            );
        };
        let Some(fragment) = line.fragments.get(coordinate.fragment_index) else {
            return (
                FfiLineFragmentLookup::LineOnly,
                crate::used_values::FfiCssPixelPoint::default(),
            );
        };
        let (x, y) = fragment.offset();
        (
            FfiLineFragmentLookup::Found,
            crate::used_values::FfiCssPixelPoint { x, y },
        )
    }

    fn commit_subtree(
        &self,
        node: *mut c_void,
        parent_paintable: *mut c_void,
        insert_before_paintable: *mut c_void,
        callbacks: &FfiLayoutFcCallbacks,
        sink: &FfiCommitSink,
    ) {
        let layout_index = self.optional_layout_index(callbacks, node);
        let used = layout_index.and_then(|index| self.used_values_by_index(index));
        let abspos_layout_inputs = layout_index
            .and_then(|index| self.used_values_rare_data(index))
            .and_then(|rare| rare.abspos_layout_inputs);
        if used.is_some() {
            callbacks.set_saved_abspos_layout_inputs(node, abspos_layout_inputs);
        }
        // SAFETY: The C++ sink owns paintables and copies every plain-data
        // input synchronously.
        let paintable = unsafe { (sink.prepare_node)(sink.context, node, used.is_some()) };

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
                        content_inline_size: used.content_inline_size.get(),
                        content_block_size: used.content_block_size.get(),
                        margin_left: used.margin_left.get(),
                        margin_right: used.margin_right.get(),
                        margin_top: used.margin_top.get(),
                        margin_bottom: used.margin_bottom.get(),
                        border_left: used.border_left.get(),
                        border_right: used.border_right.get(),
                        border_top: used.border_top.get(),
                        border_bottom: used.border_bottom.get(),
                        padding_left: used.padding_left.get(),
                        padding_right: used.padding_right.get(),
                        padding_top: used.padding_top.get(),
                        padding_bottom: used.padding_bottom.get(),
                        inset_left: used.inset_left.get(),
                        inset_right: used.inset_right.get(),
                        inset_top: used.inset_top.get(),
                        inset_bottom: used.inset_bottom.get(),
                    },
                );
            }

            let (line_fragment_lookup, line_fragment_offset) = self.line_fragment_lookup(callbacks, used);
            let committed_offset = FfiCommittedOffset {
                offset: used.content_offset.get(),
                inset_left: used.inset_left.get(),
                inset_top: used.inset_top.get(),
                materialized_from_paintable: used.materialized_from_paintable.get(),
                has_containing_line_box_fragment: used.has_containing_line_box_fragment.get(),
                containing_line_box_index: used.containing_line_box_fragment.get().line_box_index,
                line_fragment_lookup,
                line_fragment_offset,
            };

            let (override_borders, table_cell_coordinates) =
                self.used_values_rare_data(layout_index).map_or((None, None), |rare| {
                    (rare.override_borders_data, rare.table_cell_coordinates)
                });
            unsafe {
                if let Some(borders) = override_borders {
                    (sink.set_override_borders)(sink.context, paintable, borders);
                }
                if let Some(coordinates) = table_cell_coordinates {
                    (sink.set_table_cell_coordinates)(sink.context, paintable, coordinates);
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

            let (transforms, path, grid_data, flex_data, tracks) = self
                .used_values_rare_data_mut_if_present(layout_index)
                .map_or((None, None, None, None, None), |mut rare| {
                    (
                        rare.computed_svg_transforms,
                        rare.computed_svg_path.as_mut().map(RetainedLayoutHandle::take),
                        rare.grid_layout_data.as_mut().map(RetainedLayoutHandle::take),
                        rare.flex_layout_data.as_mut().map(RetainedLayoutHandle::take),
                        rare.used_grid_tracks.as_mut().map(RetainedLayoutHandle::take),
                    )
                });
            unsafe {
                if let Some(transforms) = transforms {
                    (sink.set_computed_svg_transforms)(sink.context, paintable, transforms);
                }
                if let Some(path) = path {
                    (sink.set_computed_svg_path)(sink.context, paintable, path);
                }
                if let Some(data) = grid_data {
                    (sink.set_grid_layout_data)(sink.context, paintable, data);
                }
                if let Some(data) = flex_data {
                    (sink.set_flex_layout_data)(sink.context, paintable, data);
                }
                if let Some(tracks) = tracks {
                    (sink.set_used_grid_tracks)(sink.context, paintable, tracks);
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

        let mut child = callbacks.first_child(node);
        while !child.is_null() {
            let next = callbacks.next_sibling(child);
            self.commit_subtree(child, result.paintable_for_children, null_mut(), callbacks, sink);
            child = next;
        }
    }

    pub(crate) fn commit_at(
        &self,
        root: *mut c_void,
        parent_paintable: *mut c_void,
        insert_before_paintable: *mut c_void,
        callbacks: &FfiLayoutFcCallbacks,
        sink: &FfiCommitSink,
    ) {
        self.commit_subtree(root, parent_paintable, insert_before_paintable, callbacks, sink);

        // Relative inline ancestor offsets require the complete paint tree.
        // Keep the pass paintable-only on the C++ side, but preserve the used
        // values store's original page/index iteration order.
        self.used_values.for_each(|used| unsafe {
            (sink.resolve_relative_positions)(sink.context, used.node);
        });
    }

    pub(crate) fn commit_replacing(
        &self,
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
    fn used_values_rare_data_mut_if_present(&self, layout_index: u32) -> Option<RefMut<'_, UsedValuesRareData>> {
        self.used_values_rare_data.get(layout_index).map(RefCell::borrow_mut)
    }
}

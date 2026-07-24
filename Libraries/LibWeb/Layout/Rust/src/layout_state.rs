/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::abort_on_panic;
use crate::box_facts::FfiLayoutBoxFacts;
use crate::fc::grid::facts::GridStyleFacts;
use crate::fc::inline::text::TextNodeFacts;
use crate::fc::inline::{line_box::LineBoxData, pieces::InlineBoxPieceData};
use crate::fc::{FfiLayoutFcCallbacks, FfiTableBoxFacts};
use crate::ffi_stats::{FfiOp, bump};
use crate::geometry::LogicalRect;
use crate::style_facts::FfiStyleFacts;
use crate::used_values::UsedValuesCore;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr::null_mut;

unsafe extern "C" {
    fn ladybird_layout_box_layout_index(box_pointer: *mut c_void) -> u32;
}

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

struct PagedStore<T> {
    pages: Vec<Option<Box<Page<T>>>>,
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
    fn ensure_capacity(&mut self, count: u32) {
        let page_count = ((count as usize) + PAGE_SIZE - 1) >> PAGE_BITS;
        self.pages.resize_with(page_count, || None);
    }

    fn get(&self, index: u32) -> *mut T {
        let index = index as usize;
        let page_index = index >> PAGE_BITS;
        let Some(Some(page)) = self.pages.get(page_index) else {
            return null_mut();
        };
        page.entries[index & PAGE_MASK]
    }

    fn allocate(&mut self, index: u32, value: T) -> *mut T {
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
    fn for_each(&self, mut callback: impl FnMut(&T)) {
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

#[derive(Clone, Copy)]
struct ContainedAbsposChild {
    child_box: *mut c_void,
    child_layout_index: u32,
    static_position_rect: FfiStaticPositionRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FfiContainedAbsposChild {
    pub child_box: *mut c_void,
    pub static_position_rect: FfiStaticPositionRect,
}

#[derive(Default)]
#[allow(dead_code)]
pub(crate) struct LayoutState {
    used_values: PagedStore<UsedValuesCore>,
    contained_abspos_children: HashMap<usize, Vec<ContainedAbsposChild>>,
    style_facts: PagedStore<FfiStyleFacts>,
    box_facts: PagedStore<FfiLayoutBoxFacts>,
    table_facts: PagedStore<FfiTableBoxFacts>,
    grid_facts: PagedStore<GridStyleFacts>,
    text_facts: HashMap<usize, TextNodeFacts>,
    line_data: PagedStore<LineData>,
    layout_indices_by_node: HashMap<usize, u32>,
}

#[derive(Default)]
#[allow(dead_code)]
pub(crate) struct LineData {
    pub(crate) line_boxes: Vec<LineBoxData>,
    pub(crate) inline_box_pieces: Vec<InlineBoxPieceData>,
}

impl Drop for LayoutState {
    fn drop(&mut self) {
        self.style_facts.for_each(|facts| facts.release_calc_handles());
    }
}

#[allow(dead_code)]
impl LayoutState {
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
                return unsafe { *facts };
            }
        }
        self.build_box_facts(callbacks, node)
    }

    pub(crate) fn style_facts(&mut self, callbacks: &FfiLayoutFcCallbacks, node: *mut c_void) -> FfiStyleFacts {
        let index = self.layout_index(callbacks, node);
        let facts = self.style_facts.get(index);
        if !facts.is_null() {
            bump(FfiOp::StyleFactsCacheHit);
            // SAFETY: Non-null store entries remain valid for the state.
            return unsafe { *facts };
        }
        // SAFETY: See build_box_facts().
        let facts = unsafe { (callbacks.build_style_facts)(callbacks.context, node) };
        self.style_facts.allocate(index, facts);
        facts
    }

    pub(crate) fn replace_resolved_anchor_insets(
        &mut self,
        callbacks: &FfiLayoutFcCallbacks,
        node: *mut c_void,
        resolved: crate::fc::abspos::FfiResolvedAnchorInsets,
    ) {
        let index = self.layout_index(callbacks, node);
        let facts = self.style_facts.get(index);
        assert!(!facts.is_null());
        // SAFETY: The style-facts entry is stable and layout mutates it
        // serially when anchor functions become plain used-value insets.
        let facts = unsafe { &mut *facts };
        let replace =
            |slot: &mut crate::style_facts::FfiSizeValue, is_auto: bool, value: crate::css_pixels::CssPixels| {
                slot.release_calc_handle();
                *slot = if is_auto {
                    crate::style_facts::FfiSizeValue::auto_value()
                } else {
                    crate::style_facts::FfiSizeValue::px_value(value)
                };
            };
        if resolved.resolves_top {
            replace(&mut facts.inset_top, resolved.top_is_auto, resolved.top);
        }
        if resolved.resolves_right {
            replace(&mut facts.inset_right, resolved.right_is_auto, resolved.right);
        }
        if resolved.resolves_bottom {
            replace(&mut facts.inset_bottom, resolved.bottom_is_auto, resolved.bottom);
        }
        if resolved.resolves_left {
            replace(&mut facts.inset_left, resolved.left_is_auto, resolved.left);
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
}

pub(crate) fn state_mut(state: *mut c_void) -> &'static mut LayoutState {
    assert!(!state.is_null());
    // SAFETY: State pointers are created by rust_layout_state_create, remain
    // exclusively owned by the C++ LayoutState wrapper, and are destroyed
    // only after the final call using them.
    unsafe { &mut *state.cast::<LayoutState>() }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_state_create() -> *mut c_void {
    abort_on_panic(|| {
        bump(FfiOp::StateCreate);
        Box::into_raw(Box::new(LayoutState::default())).cast()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_state_destroy(state: *mut c_void) {
    abort_on_panic(|| {
        bump(FfiOp::StateDestroy);
        assert!(!state.is_null());
        // SAFETY: The pointer was returned by rust_layout_state_create and
        // ownership is transferred back exactly once by the C++ destructor.
        unsafe {
            drop(Box::from_raw(state.cast::<LayoutState>()));
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_state_ensure_capacity(state: *mut c_void, count: u32) {
    abort_on_panic(|| {
        bump(FfiOp::StateEnsureCapacity);
        state_mut(state).used_values.ensure_capacity(count);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_state_create_used_values(state: *mut c_void, index: u32) -> *mut UsedValuesCore {
    abort_on_panic(|| {
        bump(FfiOp::UsedValuesCreate);
        state_mut(state).used_values.allocate(index, UsedValuesCore::default())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_state_get_used_values(state: *mut c_void, index: u32) -> *mut UsedValuesCore {
    abort_on_panic(|| {
        bump(FfiOp::UsedValuesGet);
        state_mut(state).used_values.get(index)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_register_contained_abspos_child(
    state: *mut c_void,
    target_box: *mut c_void,
    child_box: *mut c_void,
    static_position_rect: FfiStaticPositionRect,
) {
    abort_on_panic(|| {
        bump(FfiOp::AbsposRegister);
        let state = state_mut(state);
        assert!(!child_box.is_null());
        bump(FfiOp::AbsposLayoutIndexCallback);
        // SAFETY: The pointer is supplied by the C++ caller and remains a
        // live Layout::Box for the duration of this synchronous callback.
        let child_layout_index = unsafe { ladybird_layout_box_layout_index(child_box) };
        state.register_contained_abspos_child(target_box, child_box, child_layout_index, static_position_rect);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_take_next_contained_abspos_child(
    state: *mut c_void,
    target_box: *mut c_void,
    out: *mut FfiContainedAbsposChild,
) -> bool {
    abort_on_panic(|| {
        bump(FfiOp::AbsposTake);
        assert!(!out.is_null());
        let state = state_mut(state);
        let Some(child) = state.take_next_contained_abspos_child(target_box) else {
            return false;
        };
        // SAFETY: C++ supplies a valid writable out pointer whenever it calls
        // this function. It is written only on the true return path.
        unsafe {
            out.write(child);
        }
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paged_store_allocates_and_gets_sparse_indices() {
        let mut store = PagedStore::<u32>::default();
        assert!(store.get(1).is_null());
        let first = store.allocate(1, 11u32);
        let distant = store.allocate(63, 22u32);
        assert_eq!(store.get(1), first);
        assert_eq!(store.get(63), distant);
        assert!(store.get(2).is_null());
        // SAFETY: Both pointers refer to initialized entries owned by store.
        unsafe {
            assert_eq!(*first, 11);
            assert_eq!(*distant, 22);
        }
    }

    #[test]
    fn ensure_capacity_preallocates_only_the_top_level_table() {
        let mut store = PagedStore::<u32>::default();
        store.ensure_capacity(33);
        assert_eq!(store.pages.len(), 3);
        assert!(store.pages.iter().all(Option::is_none));
        store.allocate(32, 7);
        assert!(store.pages[2].is_some());
        assert!(store.pages[0].is_none());
    }

    #[test]
    fn allocation_grows_beyond_ensured_capacity() {
        let mut store = PagedStore::default();
        store.ensure_capacity(1);
        store.allocate(80, 5u32);
        assert_eq!(store.pages.len(), 6);
        // SAFETY: The returned entry is initialized and owned by store.
        unsafe {
            assert_eq!(*store.get(80), 5);
        }
    }

    #[test]
    fn iteration_follows_page_and_entry_order() {
        let mut store = PagedStore::default();
        store.allocate(31, 31u32);
        store.allocate(1, 1u32);
        store.allocate(17, 17u32);
        let mut values = Vec::new();
        store.for_each(|value| values.push(*value));
        assert_eq!(values, [1, 17, 31]);
    }

    #[test]
    #[should_panic(expected = "assertion failed: entry.is_null()")]
    fn duplicate_allocation_is_rejected() {
        let mut store = PagedStore::default();
        store.allocate(4, 1u32);
        store.allocate(4, 2u32);
    }
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Rust values in cells of their own, which LibGC allocates from the heap of the VM, traces through the value's
//! Trace implementation and destroys by dropping the value.

use core::ffi::{c_char, c_void};
use core::marker::PhantomData;
use core::ops::Deref;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::abi::GCVisitor;
use crate::gc::capi::{
    GC_CELL_KIND_OTHER, GC_CELL_STATE_LIVE, GCAllocator, GCCellTypeInfo, GCLayout, gc_allocator_create, gc_get_layout,
    gc_heap_allocate_cell,
};
use crate::gc::visitor::{Trace, Visitor};
use crate::layout::{
    JS_LAYOUT_CELL_CLASS_OFFSET, JS_LAYOUT_CELL_KIND_OFFSET, JS_LAYOUT_CELL_MARK_OFFSET, JS_LAYOUT_CELL_STATE_OFFSET,
};
use crate::vm::Vm;

/// The bytes LibGC shares with every cell. The first word is the vtable pointer of a C++ cell, which LibGC never
/// reads; a Rust cell keeps its class there.
#[repr(C, packed)]
struct CellHeader {
    class: *const CellClass,
    mark: bool,
    state: u8,
    kind: u8,
}

const _: () = assert!(core::mem::offset_of!(CellHeader, class) == JS_LAYOUT_CELL_CLASS_OFFSET);
const _: () = assert!(core::mem::offset_of!(CellHeader, mark) == JS_LAYOUT_CELL_MARK_OFFSET);
const _: () = assert!(core::mem::offset_of!(CellHeader, state) == JS_LAYOUT_CELL_STATE_OFFSET);
const _: () = assert!(core::mem::offset_of!(CellHeader, kind) == JS_LAYOUT_CELL_KIND_OFFSET);

/// The class of a Rust cell, which its first word points to. It starts with the type info LibGC dispatches through.
#[repr(C)]
struct CellClass {
    type_info: GCCellTypeInfo,
    name: &'static str,
}

#[repr(C)]
struct RustCell<T> {
    header: CellHeader,
    value: T,
}

/// LibGC hands out cells in the slots of fixed-size blocks, whose free list needs room for its own entry in each.
const MIN_CELL_SIZE: usize = 24;
const MIN_CELL_ALIGNMENT: usize = 8;
const MAX_CELL_ALIGNMENT: usize = 16;

const fn cell_size<T>() -> usize {
    let size = size_of::<RustCell<T>>().next_multiple_of(MIN_CELL_ALIGNMENT);
    if size < MIN_CELL_SIZE { MIN_CELL_SIZE } else { size }
}

const fn cell_alignment<T>() -> usize {
    let alignment = align_of::<RustCell<T>>();
    if alignment < MIN_CELL_ALIGNMENT {
        MIN_CELL_ALIGNMENT
    } else {
        alignment
    }
}

unsafe extern "C" fn visit_edges<T: Trace>(cell: *mut c_void, visitor: *mut GCVisitor) {
    // SAFETY: LibGC passes a live cell of this type and a visitor that outlives the call.
    let (cell, mut visitor) = unsafe { (&*cell.cast::<RustCell<T>>(), Visitor::from_raw(visitor)) };
    cell.value.trace(&mut visitor);
}

unsafe extern "C" fn destroy<T>(cell: *mut c_void) {
    // SAFETY: LibGC destroys each dead cell of this type once, after which nothing reads it.
    unsafe { core::ptr::drop_in_place(&raw mut (*cell.cast::<RustCell<T>>()).value) };
}

unsafe extern "C" fn class_name(cell: *const c_void, length: *mut usize) -> *const c_char {
    // SAFETY: LibGC passes a live Rust cell, whose first word is its class.
    let name = unsafe { (*(*cell.cast::<CellHeader>()).class).name };
    // SAFETY: LibGC passes a length to write to.
    unsafe { length.write(name.len()) };
    name.as_ptr().cast()
}

/// The type of the cells that hold values of type T, which a static of the embedder defines once:
///
/// ```ignore
/// static THING_CELL_TYPE: CellType<Thing> = CellType::new("Thing");
/// ```
pub struct CellType<T: 'static> {
    class: CellClass,
    allocator: AtomicPtr<GCAllocator>,
    _type: PhantomData<fn() -> T>,
}

impl<T: Trace> CellType<T> {
    pub const fn new(name: &'static str) -> Self {
        assert!(cell_alignment::<T>() <= MAX_CELL_ALIGNMENT);
        Self {
            class: CellClass {
                type_info: GCCellTypeInfo {
                    cell_size: cell_size::<T>() as u32,
                    alignment: cell_alignment::<T>() as u32,
                    kind: GC_CELL_KIND_OTHER,
                    visit_edges: Some(visit_edges::<T>),
                    finalize: None,
                    destroy: if core::mem::needs_drop::<T>() {
                        Some(destroy::<T>)
                    } else {
                        None
                    },
                    external_memory_size: None,
                    class_name: Some(class_name),
                },
                name,
            },
            allocator: AtomicPtr::new(core::ptr::null_mut()),
            _type: PhantomData,
        }
    }

    /// The allocator of the type, which the first allocation creates. Every heap of a process is used from one thread,
    /// so the allocator is never created twice.
    fn allocator(&'static self) -> *mut GCAllocator {
        let allocator = self.allocator.load(Ordering::Relaxed);
        if !allocator.is_null() {
            return allocator;
        }
        check_layout_against_libgc::<T>();
        // SAFETY: The type info and the name are static, so they outlive the allocator.
        let allocator = unsafe {
            gc_allocator_create(
                &raw const self.class.type_info,
                self.class.name.as_ptr().cast(),
                self.class.name.len(),
            )
        };
        assert!(
            !allocator.is_null(),
            "LibGC creates the allocator of {}",
            self.class.name
        );
        self.allocator.store(allocator, Ordering::Relaxed);
        allocator
    }

    /// Moves the value into a new cell of the VM's heap. Allocating can collect garbage, during which the cells the
    /// value holds stay alive as long as the value is on the stack.
    pub fn allocate(&'static self, vm: Vm, value: T) -> Gc<T> {
        let allocator = self.allocator();
        let mut must_mark = false;
        // SAFETY: The heap and the allocator are live, and the allocator is of this type's size.
        let storage = unsafe { gc_heap_allocate_cell(vm.heap().as_ptr(), allocator, &raw mut must_mark) };
        let storage = NonNull::new(storage.cast::<RustCell<T>>()).expect("LibGC allocates the cell");
        // SAFETY: LibGC returned uninitialized storage of the size and alignment of the cell, which nothing can
        //         collect before this write.
        unsafe {
            storage.write(RustCell {
                header: CellHeader {
                    class: &raw const self.class,
                    mark: must_mark,
                    state: GC_CELL_STATE_LIVE,
                    kind: GC_CELL_KIND_OTHER,
                },
                value,
            });
        }
        Gc { cell: storage }
    }

    /// The cell as a cell of this type, or None if it is a cell of another type.
    ///
    /// # Safety
    ///
    /// `cell` must be null or the address of a live cell.
    pub unsafe fn downcast(&'static self, cell: *mut c_void) -> Option<Gc<T>> {
        let cell = NonNull::new(cell)?;
        // SAFETY: The first word of every cell is readable. Only a cell of this type holds this class there.
        let class = unsafe { cell.cast::<*const CellClass>().read() };
        core::ptr::eq(class, &raw const self.class).then_some(Gc { cell: cell.cast() })
    }
}

/// Checks that the cell header and the cell sizes agree with the LibGC that the process uses.
fn check_layout_against_libgc<T>() {
    let mut layout = GCLayout::default();
    // SAFETY: The layout is valid for writes.
    unsafe { gc_get_layout(&raw mut layout) };
    assert!(layout.cell_mark_offset as usize == JS_LAYOUT_CELL_MARK_OFFSET);
    assert!(layout.cell_state_offset as usize == JS_LAYOUT_CELL_STATE_OFFSET);
    assert!(layout.cell_kind_offset as usize == JS_LAYOUT_CELL_KIND_OFFSET);
    assert!(layout.cell_type_info_size as usize == size_of::<GCCellTypeInfo>());
    assert!(layout.min_cell_size as usize <= cell_size::<T>());
    assert!(layout.max_cell_size as usize >= cell_size::<T>());
    assert!(layout.max_cell_alignment as usize >= cell_alignment::<T>());
}

/// A cell that holds a value of type T. Like a JS value, it keeps the cell alive only while it is on the stack or in
/// something that traces it.
pub struct Gc<T> {
    cell: NonNull<RustCell<T>>,
}

impl<T> Clone for Gc<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Gc<T> {}

impl<T> Gc<T> {
    /// The address of the cell, which a C++ GC::Cell pointer can hold.
    pub fn as_cell(self) -> *mut c_void {
        self.cell.as_ptr().cast()
    }
}

impl<T> Deref for Gc<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: The cell is alive while something keeps this handle on the stack or traces it.
        unsafe { &(*self.cell.as_ptr()).value }
    }
}

// SAFETY: Visits the cell, which traces its value in turn.
unsafe impl<T> Trace for Gc<T> {
    fn trace(&self, visitor: &mut Visitor) {
        // SAFETY: The handle is of a live cell.
        unsafe { visitor.visit_cell(self.as_cell()) };
    }
}

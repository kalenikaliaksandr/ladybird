/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::abort_on_panic;
use crate::node_data::NodeData;
use crate::node_data::NodeSlotId;
use std::array;
use std::ffi::c_void;
use std::thread;

const SLOTS_PER_CHUNK: usize = 256;

#[derive(Default)]
struct NodeSlot {
    data: NodeData,
    generation: u32,
    occupied: bool,
}

struct LayoutNodeArena {
    chunks: Vec<Box<[NodeSlot; SLOTS_PER_CHUNK]>>,
    free_list: Vec<u32>,
    next_index: u32,
    live_count: u32,
    owner_thread: thread::ThreadId,
}

impl LayoutNodeArena {
    fn new() -> Self {
        Self {
            chunks: Vec::new(),
            free_list: Vec::new(),
            next_index: 0,
            live_count: 0,
            owner_thread: thread::current().id(),
        }
    }

    fn assert_owner_thread(&self) {
        debug_assert_eq!(self.owner_thread, thread::current().id());
    }

    fn allocate(&mut self) -> NodeAllocation {
        self.assert_owner_thread();

        let index = if let Some(index) = self.free_list.pop() {
            index
        } else {
            let index = self.next_index;
            if (index as usize).is_multiple_of(SLOTS_PER_CHUNK) {
                self.chunks.push(Box::new(array::from_fn(|_| NodeSlot::default())));
            }
            self.next_index = self
                .next_index
                .checked_add(1)
                .expect("layout node arena exhausted its slot ID space");
            index
        };

        self.live_count = self
            .live_count
            .checked_add(1)
            .expect("layout node arena live count overflowed");

        let slot = self.slot_mut(index);
        assert!(!slot.occupied, "layout node arena allocated a live slot");
        slot.data = NodeData::default();
        slot.generation = slot.generation.wrapping_add(1);
        if slot.generation == 0 {
            slot.generation = 1;
        }
        slot.occupied = true;

        NodeAllocation {
            slot: NodeSlotId { index },
            data: &raw mut slot.data,
            generation: slot.generation,
        }
    }

    fn free(&mut self, id: NodeSlotId, generation: u32) {
        self.assert_owner_thread();

        {
            let slot = self.slot_mut(id.index);
            assert!(slot.occupied, "layout node arena freed an unused slot");
            assert_eq!(
                slot.generation, generation,
                "layout node arena freed a stale slot generation"
            );
            slot.occupied = false;
            slot.data = NodeData::default();
        }

        self.live_count = self
            .live_count
            .checked_sub(1)
            .expect("layout node arena live count underflowed");
        self.free_list.push(id.index);
    }

    fn slot_mut(&mut self, index: u32) -> &mut NodeSlot {
        let index = index as usize;
        let chunk = index / SLOTS_PER_CHUNK;
        let offset = index % SLOTS_PER_CHUNK;
        self.chunks
            .get_mut(chunk)
            .and_then(|chunk| chunk.get_mut(offset))
            .expect("invalid layout node arena slot ID")
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct NodeAllocation {
    pub slot: NodeSlotId,
    pub data: *mut NodeData,
    pub generation: u32,
}

#[unsafe(no_mangle)]
pub extern "C" fn layout_arena_create() -> *mut c_void {
    abort_on_panic(|| Box::into_raw(Box::new(LayoutNodeArena::new())).cast())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn layout_arena_destroy(arena: *mut c_void) {
    abort_on_panic(|| {
        assert!(!arena.is_null(), "layout node arena handle is null");
        // SAFETY: The handle came from layout_arena_create and ownership is
        // transferred back exactly once by the C++ RAII wrapper.
        let arena = unsafe { Box::from_raw(arena.cast::<LayoutNodeArena>()) };
        arena.assert_owner_thread();
        assert_eq!(arena.live_count, 0, "layout node arena destroyed with live slots");
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn layout_arena_allocate(arena: *mut c_void) -> NodeAllocation {
    abort_on_panic(|| {
        assert!(!arena.is_null(), "layout node arena handle is null");
        // SAFETY: The C++ wrapper keeps the arena alive for this call and
        // serializes all access on the document thread.
        unsafe { &mut *arena.cast::<LayoutNodeArena>() }.allocate()
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn layout_arena_free(arena: *mut c_void, id: NodeSlotId, generation: u32) {
    abort_on_panic(|| {
        assert!(!arena.is_null(), "layout node arena handle is null");
        // SAFETY: The C++ wrapper keeps the arena alive for this call and
        // serializes all access on the document thread.
        unsafe { &mut *arena.cast::<LayoutNodeArena>() }.free(id, generation);
    });
}

#[cfg(test)]
mod tests {
    use super::LayoutNodeArena;
    use super::SLOTS_PER_CHUNK;
    use crate::node_data::NodeData;

    #[test]
    fn node_data_addresses_remain_stable_when_chunks_are_added() {
        let mut arena = LayoutNodeArena::new();
        let first = arena.allocate();
        let first_data = first.data;

        let mut allocations = Vec::new();
        for _ in 0..SLOTS_PER_CHUNK * 2 {
            allocations.push(arena.allocate());
        }

        let current_data = &mut arena.slot_mut(first.slot.index).data as *mut NodeData;
        assert_eq!(first_data, current_data);
        // SAFETY: The first allocation is still live, and the comparison above
        // confirms that its pointer still addresses the arena slot.
        unsafe {
            (*first_data).layout_index = 42;
        }
        assert_eq!(arena.slot_mut(first.slot.index).data.layout_index, 42);
        arena.free(first.slot, first.generation);
        for allocation in allocations {
            arena.free(allocation.slot, allocation.generation);
        }
    }

    #[test]
    fn freed_slots_are_reused_with_a_new_generation() {
        let mut arena = LayoutNodeArena::new();
        let first = arena.allocate();
        arena.free(first.slot, first.generation);

        let second = arena.allocate();
        assert_eq!(second.slot, first.slot);
        assert_ne!(second.generation, first.generation);
        arena.free(second.slot, second.generation);
    }
}

use crate::layout_node_arena::{Chunk, LayoutNodeArena, SLOTS_PER_CHUNK};

#[test]
fn node_data_addresses_remain_stable_when_chunks_are_added() {
    let mut arena = LayoutNodeArena::new();
    let first = arena.allocate();
    let first_data = first.data;

    let mut allocations = Vec::new();
    for _ in 0..SLOTS_PER_CHUNK * 2 {
        allocations.push(arena.allocate());
    }

    assert_eq!(first_data, arena.data(first.slot));
    // SAFETY: The first allocation is still live, and the comparison above
    // confirms that its pointer still addresses the arena slot.
    unsafe {
        (*first_data).layout_index = 42;
        assert_eq!((*arena.data(first.slot)).layout_index, 42);
    }
    arena.free(first.slot, first.generation);
    for allocation in allocations {
        arena.free(allocation.slot, allocation.generation);
    }
}

#[test]
fn node_data_slots_are_cache_line_aligned() {
    assert_eq!(align_of::<Chunk>() % 64, 0);
    let mut arena = LayoutNodeArena::new();
    let allocation = arena.allocate();
    assert_eq!(allocation.data as usize % 64, 0);
    arena.free(allocation.slot, allocation.generation);
}

#[test]
fn freed_slots_are_reused_with_a_new_generation() {
    let mut arena = LayoutNodeArena::new();
    let first = arena.allocate();
    arena.free(first.slot, first.generation);

    let second = arena.allocate();
    assert_eq!(second.slot.slot_index(), first.slot.slot_index());
    assert_ne!(second.slot, first.slot);
    assert_ne!(second.generation, first.generation);
    arena.free(second.slot, second.generation);
}

#[test]
fn stale_slot_ids_do_not_resolve_to_a_new_occupant() {
    let mut arena = LayoutNodeArena::new();
    let first = arena.allocate();
    arena.free(first.slot, first.generation);
    let second = arena.allocate();

    let stale_read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| arena.data(first.slot)));
    assert!(stale_read.is_err());
    arena.free(second.slot, second.generation);
}

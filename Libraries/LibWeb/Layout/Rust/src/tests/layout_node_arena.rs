use crate::css_pixels::CssPixels;
use crate::layout_node_arena::{
    Chunk, IntrinsicSizeCacheKey, IntrinsicSizeCacheKind, LayoutNodeArena, SLOTS_PER_CHUNK,
};
use crate::layout_state::{
    AbsposAlignment, AbsposAxisMode, AbsposContainingBlockInfo, AbsposLayoutInputs, StaticPositionAlignment,
    StaticPositionRect,
};
use crate::node_data::{NodeFlag, NodeKind};

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

#[test]
fn intrinsic_size_cache_validates_epoch_and_generation() {
    let mut arena = LayoutNodeArena::new();
    let first = arena.allocate();
    let key = IntrinsicSizeCacheKey {
        measured_at_inline_size: Some(CssPixels::from_raw(64)),
        ..Default::default()
    };
    let value = CssPixels::from_raw(128);

    // SAFETY: The allocation remains live until it is explicitly freed below.
    let first_data = unsafe { &mut *first.data };
    arena.intrinsic_size_cache_put(first_data, IntrinsicSizeCacheKind::MinContentBlock, key, value);
    assert_eq!(
        arena.intrinsic_size_cache_get(first_data, IntrinsicSizeCacheKind::MinContentBlock, key),
        Some(value)
    );

    first_data.intrinsic_cache_epoch += 1;
    assert_eq!(
        arena.intrinsic_size_cache_get(first_data, IntrinsicSizeCacheKind::MinContentBlock, key),
        None
    );
    arena.free(first.slot, first.generation);

    let second = arena.allocate();
    assert_eq!(second.slot.slot_index(), first.slot.slot_index());
    assert_ne!(second.slot, first.slot);
    // SAFETY: The second allocation is live and reuses the first allocation's slot.
    let second_data = unsafe { &*second.data };
    assert_eq!(
        arena.intrinsic_size_cache_get(second_data, IntrinsicSizeCacheKind::MinContentBlock, key),
        None
    );
    arena.free(second.slot, second.generation);
}

#[test]
fn saved_abspos_inputs_transfer_and_validate_generation() {
    let mut arena = LayoutNodeArena::new();
    let old = arena.allocate();
    let new = arena.allocate();
    // SAFETY: Both allocations remain live.
    unsafe {
        (*old.data).kind = NodeKind::Box;
        (*new.data).kind = NodeKind::Box;
    }
    let inputs = AbsposLayoutInputs {
        static_position_rect: StaticPositionRect {
            rect: Default::default(),
            inline_alignment: StaticPositionAlignment::Center,
            block_alignment: StaticPositionAlignment::End,
            alignment_derives_from_own_computed_values: true,
        },
        containing_block_info: AbsposContainingBlockInfo {
            rect: Default::default(),
            inline_axis_mode: AbsposAxisMode::StaticPosition,
            block_axis_mode: AbsposAxisMode::InsetFromRect,
            has_inline_alignment: true,
            inline_alignment: AbsposAlignment::Center,
            has_block_alignment: false,
            block_alignment: AbsposAlignment::Normal,
            derives_from_own_computed_values: true,
        },
    };

    arena.set_saved_abspos_layout_inputs(old.data, Some(inputs));
    assert_eq!(arena.saved_abspos_layout_inputs(old.data), Some(inputs));
    // SAFETY: Both allocations remain live.
    unsafe {
        assert_ne!((*old.data).flags & NodeFlag::HasSavedAbsposLayoutInputs as u32, 0);
        assert_ne!(
            (*old.data).flags & NodeFlag::SavedAbsposCbDerivesFromOwnComputedValues as u32,
            0
        );
        assert_ne!(
            (*old.data).flags & NodeFlag::SavedAbsposAlignmentDerivesFromOwnComputedValues as u32,
            0
        );
    }

    arena.transfer_saved_abspos_layout_inputs(old.slot, new.slot);
    assert_eq!(arena.saved_abspos_layout_inputs(new.data), Some(inputs));

    arena.set_saved_abspos_layout_inputs(old.data, None);
    assert_eq!(arena.saved_abspos_layout_inputs(old.data), None);
    // SAFETY: The old allocation remains live.
    unsafe {
        let saved_abspos_flags = NodeFlag::HasSavedAbsposLayoutInputs as u32
            | NodeFlag::SavedAbsposCbDerivesFromOwnComputedValues as u32
            | NodeFlag::SavedAbsposAlignmentDerivesFromOwnComputedValues as u32;
        assert_eq!((*old.data).flags & saved_abspos_flags, 0);
    }
    arena.free(old.slot, old.generation);

    let reused = arena.allocate();
    assert_eq!(reused.slot.slot_index(), old.slot.slot_index());
    assert_ne!(reused.slot, old.slot);
    assert_eq!(arena.saved_abspos_layout_inputs(reused.data), None);
    arena.free(reused.slot, reused.generation);
    arena.free(new.slot, new.generation);
}

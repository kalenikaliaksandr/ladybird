use crate::node_data::{MAX_NODE_SLOT_COUNT, NodeData, NodeFlag, NodeKind, NodeSlotId};

#[test]
fn node_kind_has_a_stable_default_and_byte_width() {
    assert_eq!(std::mem::size_of::<NodeKind>(), 1);
    assert_eq!(NodeData::default().kind, NodeKind::Unset);
}

#[test]
fn intrinsic_cache_epoch_uses_existing_node_data_padding() {
    assert_eq!(std::mem::size_of::<NodeData>(), 64);
    assert_eq!(std::mem::offset_of!(NodeData, intrinsic_cache_epoch), 30);
    assert_eq!(std::mem::offset_of!(NodeData, flags), 32);
    assert_eq!(std::mem::offset_of!(NodeData, slot_generation), 47);
    assert_eq!(std::mem::offset_of!(NodeData, style), 48);
    assert_eq!(std::mem::offset_of!(NodeData, shell), 56);
}

#[test]
fn node_slot_id_packs_a_24_bit_index_and_an_8_bit_generation() {
    let id = NodeSlotId::new(MAX_NODE_SLOT_COUNT - 1, u8::MAX);
    assert_eq!(id.slot_index(), MAX_NODE_SLOT_COUNT - 1);
    assert_eq!(id.generation(), u8::MAX);
    assert_ne!(id, NodeSlotId::INVALID);
}

#[test]
fn saved_abspos_flags_use_previously_unassigned_bits() {
    assert_eq!(NodeFlag::IsReplacedElement as u32, 1 << 12);
    assert_eq!(NodeFlag::HasSavedAbsposLayoutInputs as u32, 1 << 13);
    assert_eq!(NodeFlag::SavedAbsposCbDerivesFromOwnComputedValues as u32, 1 << 14);
    assert_eq!(
        NodeFlag::SavedAbsposAlignmentDerivesFromOwnComputedValues as u32,
        1 << 15
    );
}

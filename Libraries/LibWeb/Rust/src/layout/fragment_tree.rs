/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

pub(crate) fn shadow_fragment_diff_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("LADYBIRD_LAYOUT_SHADOW_FRAGMENTS").is_some_and(|value| value == "1"))
}

/// Immutable result of laying out one box, assembled from the box's sealed
/// used-values cells at its placement. Line data and rare payloads join in a
/// later capture stage.
pub(crate) struct Fragment {
    pub(crate) node: crate::layout::node_data::NodeSlotId,
    pub(crate) content_inline_size: CssPixels,
    pub(crate) content_block_size: CssPixels,
    pub(crate) margin_left: CssPixels,
    pub(crate) margin_right: CssPixels,
    pub(crate) margin_top: CssPixels,
    pub(crate) margin_bottom: CssPixels,
    pub(crate) border_left: CssPixels,
    pub(crate) border_right: CssPixels,
    pub(crate) border_top: CssPixels,
    pub(crate) border_bottom: CssPixels,
    pub(crate) padding_left: CssPixels,
    pub(crate) padding_right: CssPixels,
    pub(crate) padding_top: CssPixels,
    pub(crate) padding_bottom: CssPixels,
    pub(crate) table_cell_coordinates: Option<FfiTableCellCoordinates>,
    pub(crate) override_borders_data: Option<FfiBordersData>,
    /// Moved out of the record when the box is placed; the payload digests
    /// proved nothing writes them after that moment.
    pub(crate) line_data: Option<Box<LineData>>,
    pub(crate) grid_layout_data: Option<OwnedGridLayoutData>,
    pub(crate) flex_layout_data: Option<OwnedFlexLayoutData>,
    pub(crate) used_grid_tracks: Option<OwnedUsedGridTracks>,
    pub(crate) children: Vec<FragmentLink>,
}

/// One placement of a fragment. Everything the parent decides about the
/// child lives on the link: the emission offset (the placed offset with the
/// committed delta already folded in) and the inset family.
pub(crate) struct FragmentLink {
    pub(crate) node: crate::layout::node_data::NodeSlotId,
    pub(crate) fragment: Box<Fragment>,
    pub(crate) offset: FfiCssPixelPoint,
    pub(crate) inset_left: CssPixels,
    pub(crate) inset_right: CssPixels,
    pub(crate) inset_top: CssPixels,
    pub(crate) inset_bottom: CssPixels,
    /// Resolved at placement from the containing block's final line data,
    /// replacing commit's cross-node lookup.
    pub(crate) containing_line_box_index: Option<usize>,
    /// Carried for commit's transfer into the arena's saved-inputs table;
    /// written by the abspos engine just before it places the box.
    pub(crate) abspos_layout_inputs: Option<AbsposLayoutInputs>,
    /// The box had a record but was never placed; emitted at the default
    /// offset, exactly as the store-fed commit does today.
    pub(crate) is_unplaced_orphan: bool,
}

fn snapshot_link(
    node: crate::layout::node_data::NodeSlotId,
    children: Vec<FragmentLink>,
    used: &UsedValues,
    is_unplaced_orphan: bool,
    containing_line_box_index: Option<usize>,
    emission_offset: FfiCssPixelPoint,
) -> FragmentLink {
    let line_data = used.line_data.get().map(|cell| Box::new(cell.take()));
    let (table_cell_coordinates, override_borders_data, grid_layout_data, flex_layout_data, used_grid_tracks, abspos_layout_inputs) = used
        .rare_data
        .get()
        .map(|cell| {
            let mut rare = cell.borrow_mut();
            (
                rare.table_cell_coordinates,
                rare.override_borders_data,
                rare.grid_layout_data.take(),
                rare.flex_layout_data.take(),
                rare.used_grid_tracks.take(),
                rare.abspos_layout_inputs,
            )
        })
        .unwrap_or((None, None, None, None, None, None));
    FragmentLink {
        node,
        fragment: Box::new(Fragment {
            node,
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
            table_cell_coordinates,
            override_borders_data,
            line_data,
            grid_layout_data,
            flex_layout_data,
            used_grid_tracks,
            children,
        }),
        offset: emission_offset,
        inset_left: used.inset_left.get(),
        inset_right: used.inset_right.get(),
        inset_top: used.inset_top.get(),
        inset_bottom: used.inset_bottom.get(),
        containing_line_box_index,
        abspos_layout_inputs,
        is_unplaced_orphan,
    }
}

/// What a committing run hands back alongside ChildLayoutResult: the run
/// root's completed child links plus placements that could not attach to a
/// still-open frame and must ride upward.
#[derive(Default)]
pub(crate) struct PendingRunResult {
    pub(crate) root_children: Vec<FragmentLink>,
    pub(crate) late_attachments: Vec<FragmentLink>,
}

impl PendingRunResult {
    /// Total fragments in the structure, orphans and late attachments
    /// included — the presence side of the shadow diff.
    pub(crate) fn fragment_count(&self) -> usize {
        fn count(links: &[FragmentLink]) -> usize {
            links.iter().map(|link| 1 + count(&link.fragment.children)).sum()
        }
        count(&self.root_children) + count(&self.late_attachments)
    }
}

/// Flattens a pass's fragment structure into the slot-keyed index commit
/// emits from. The link↔fragment identity and one-fragment-per-slot
/// invariants are structural, so they hold in every build.
pub(crate) fn fold_commit_index(pending: &PendingRunResult) -> std::collections::HashMap<u32, &FragmentLink> {
    fn flatten<'tree>(
        links: &'tree [FragmentLink],
        flattened: &mut std::collections::HashMap<u32, &'tree FragmentLink>,
    ) {
        for link in links {
            assert!(
                link.node == link.fragment.node,
                "a fragment link and its fragment disagree about their box"
            );
            let previous = flattened.insert(link.node.slot_index(), link);
            assert!(
                previous.is_none(),
                "two fragments claim slot {}",
                link.node.slot_index()
            );
            flatten(&link.fragment.children, flattened);
        }
    }
    let mut flattened = std::collections::HashMap::new();
    flatten(&pending.root_children, &mut flattened);
    flatten(&pending.late_attachments, &mut flattened);
    flattened
}

enum FrameState {
    Pending {
        node: crate::layout::node_data::NodeSlotId,
        children: Vec<FragmentLink>,
    },
    Consumed,
}

/// Per-run collector of fragment structure. Frames are lazy pending-children
/// lists keyed by slot: a placed child links under the frame of its
/// containing block, and placing a box consumes the box's own frame (or its
/// parked child-run deposit) into an immutable Fragment. Every in-run child
/// is placed before its container, so the tree freezes bottom-up at
/// placement moments; only the run root and orphans remain at run end.
pub(crate) struct RunFragmentBuilder {
    root_node: crate::layout::node_data::NodeSlotId,
    /// Table rows, row groups, and cells have the table WRAPPER — the run
    /// root's own containing block — as their containing block, and commit
    /// emits their offsets in that space. Links naming it attach under the
    /// pending root: the wrapper never places inside this run, and its own
    /// fragment belongs to the parent run.
    root_containing_block_slot: Option<u32>,
    /// An entry accumulator collects placements whose containing blocks were
    /// placed by other runs (entry sweeps); those park as late attachments
    /// instead of fabricating frames that can never be placed here.
    is_entry_accumulator: bool,
    inner: std::cell::RefCell<RunFragmentBuilderInner>,
}

#[derive(Default)]
struct RunFragmentBuilderInner {
    frames: std::collections::HashMap<u32, FrameState>,
    deposits: std::collections::HashMap<u32, (crate::layout::node_data::NodeSlotId, PendingRunResult)>,
    /// Records this run creates and never places (inline boxes entered for
    /// their box-model metrics); swept into orphan links so the tree covers
    /// the store exactly.
    unplaced_records: Vec<crate::layout::node_data::NodeSlotId>,
    root_children: Vec<FragmentLink>,
    late_attachments: Vec<FragmentLink>,
}

impl RunFragmentBuilder {
    pub(crate) fn new(
        root_node: crate::layout::node_data::NodeSlotId,
        root_containing_block: Option<crate::layout::node_data::NodeSlotId>,
    ) -> Self {
        Self {
            root_node,
            root_containing_block_slot: root_containing_block.map(|node| node.slot_index()),
            is_entry_accumulator: false,
            inner: std::cell::RefCell::new(RunFragmentBuilderInner::default()),
        }
    }

    pub(crate) fn new_entry_accumulator(root_node: crate::layout::node_data::NodeSlotId) -> Self {
        Self {
            root_node,
            root_containing_block_slot: None,
            is_entry_accumulator: true,
            inner: std::cell::RefCell::new(RunFragmentBuilderInner::default()),
        }
    }

    /// Declares a record this run creates without ever placing it.
    pub(crate) fn note_unplaced_record(&self, node: crate::layout::node_data::NodeSlotId) {
        self.inner.borrow_mut().unplaced_records.push(node);
    }

    /// Parks a child run's returned structure until the parent places the
    /// child. The gap between a run returning and place_child is where the
    /// parent legally writes the offset family.
    pub(crate) fn deposit_child_run(&self, child: crate::layout::node_data::NodeSlotId, pending: PendingRunResult) {
        let previous = self
            .inner
            .borrow_mut()
            .deposits
            .insert(child.slot_index(), (child, pending));
        debug_assert!(previous.is_none(), "child run deposited twice before placement");
    }

    /// Records a placement: consumes the box's accumulated frame or parked
    /// deposit into a Fragment snapshotted from the sealed record, and
    /// attaches it under its containing block's frame. This is the one
    /// moment payloads may join a link.
    pub(crate) fn absorb_placement(
        &self,
        node: crate::layout::node_data::NodeSlotId,
        containing_block: Option<crate::layout::node_data::NodeSlotId>,
        used: &UsedValues,
        containing_block_is_already_placed: bool,
        containing_line_box_index: Option<usize>,
        emission_offset: FfiCssPixelPoint,
    ) {
        let mut inner = self.inner.borrow_mut();
        let slot = node.slot_index();
        let (children, mut carried_late) = match inner.frames.insert(slot, FrameState::Consumed) {
            Some(FrameState::Pending { children, .. }) => (children, Vec::new()),
            Some(FrameState::Consumed) => {
                debug_assert!(false, "a box was placed twice in one run");
                (Vec::new(), Vec::new())
            }
            None => match inner.deposits.remove(&slot) {
                Some((_, pending)) => (pending.root_children, pending.late_attachments),
                None => (Vec::new(), Vec::new()),
            },
        };
        let link = snapshot_link(node, children, used, false, containing_line_box_index, emission_offset);
        inner.late_attachments.append(&mut carried_late);
        self.attach(&mut inner, link, containing_block, containing_block_is_already_placed);
    }

    fn attach(
        &self,
        inner: &mut RunFragmentBuilderInner,
        link: FragmentLink,
        containing_block: Option<crate::layout::node_data::NodeSlotId>,
        containing_block_is_already_placed: bool,
    ) {
        let Some(containing_block) = containing_block else {
            inner.root_children.push(link);
            return;
        };
        if containing_block.slot_index() == self.root_node.slot_index()
            || Some(containing_block.slot_index()) == self.root_containing_block_slot
        {
            inner.root_children.push(link);
            return;
        }
        if let Some(state) = inner.frames.get_mut(&containing_block.slot_index()) {
            match state {
                FrameState::Pending { children, .. } => children.push(link),
                // The containing block's fragment is already sealed (a
                // drained placement targeting a placed table row) — ride
                // upward.
                FrameState::Consumed => inner.late_attachments.push(link),
            }
            return;
        }
        // A frame may only open for a containing block this run will still
        // place; a placed containing block's fragment is sealed in whatever
        // builder placed it (a drain host above a descendant run's table
        // internals, an entry sweep above the whole pass).
        if self.is_entry_accumulator || containing_block_is_already_placed {
            inner.late_attachments.push(link);
            return;
        }
        inner.frames.insert(
            containing_block.slot_index(),
            FrameState::Pending {
                node: containing_block,
                children: vec![link],
            },
        );
    }

    /// Closes the run: sweeps never-placed frames and deposits into orphan
    /// links snapshotted from their records, and returns the run root's
    /// completed structure. The builder is empty afterwards.
    pub(crate) fn take_pending_result(&self, state: &LayoutState) -> PendingRunResult {
        let mut inner = self.inner.take();
        let unplaced_records = std::mem::take(&mut inner.unplaced_records);
        for node in unplaced_records {
            let slot = node.slot_index();
            // A frame or deposit under the same slot is swept below and
            // already carries the record.
            if inner.frames.contains_key(&slot) || inner.deposits.contains_key(&slot) {
                continue;
            }
            let Some(used) = state.used_values_by_slot(slot) else {
                debug_assert!(false, "an unplaced record note has no record");
                continue;
            };
            debug_assert!(!used.has_content_offset.get(), "an unplaced record note was placed after all");
            inner
                .root_children
                .push(snapshot_link(node, Vec::new(), used, true, None, used.content_offset.get()));
        }
        let frames = std::mem::take(&mut inner.frames);
        for (slot, frame) in frames {
            let FrameState::Pending { node, children } = frame else {
                continue;
            };
            let Some(used) = state.used_values_by_slot(slot) else {
                debug_assert!(false, "a pending frame's containing block has no record");
                continue;
            };
            inner
                .root_children
                .push(snapshot_link(node, children, used, true, None, used.content_offset.get()));
        }
        let deposits = std::mem::take(&mut inner.deposits);
        for (slot, (node, pending)) in deposits {
            let Some(used) = state.used_values_by_slot(slot) else {
                debug_assert!(false, "a deposited child run's root has no record");
                continue;
            };
            inner.late_attachments.extend(pending.late_attachments);
            inner
                .root_children
                .push(snapshot_link(node, pending.root_children, used, true, None, used.content_offset.get()));
        }
        PendingRunResult {
            root_children: inner.root_children,
            late_attachments: inner.late_attachments,
        }
    }
}

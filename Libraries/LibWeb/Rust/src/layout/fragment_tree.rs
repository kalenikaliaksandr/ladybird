/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

pub(crate) fn shadow_fragment_diff_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("LADYBIRD_LAYOUT_SHADOW_FRAGMENTS").is_some_and(|value| value == "1"))
}

fn capture_shadow_fingerprints() -> bool {
    cfg!(debug_assertions) || shadow_fragment_diff_enabled()
}

/// Compact digest of a box's line data, captured when the fragment is
/// assembled and recomputed by the shadow diff at pass end: an inequality
/// means the payload mutated after the moment the coming move-into-fragment
/// stage would have taken it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LineDataFingerprint {
    line_count: usize,
    fragment_count: usize,
    piece_count: usize,
    geometry_checksum: i64,
}

pub(crate) fn line_data_fingerprint(data: &LineData) -> LineDataFingerprint {
    fn add(checksum: &mut i64, value: CssPixels) {
        *checksum = checksum.wrapping_add(value.raw_value() as i64);
    }
    let mut checksum: i64 = 0;
    let mut fragment_count = 0usize;
    for line in &data.line_boxes {
        fragment_count += line.fragments.len();
        add(&mut checksum, line.inline_length);
        add(&mut checksum, line.block_length);
        add(&mut checksum, line.block_end);
        add(&mut checksum, line.baseline);
        for fragment in &line.fragments {
            add(&mut checksum, fragment.inline_offset);
            add(&mut checksum, fragment.block_offset);
            add(&mut checksum, fragment.inline_length);
            add(&mut checksum, fragment.block_length);
            add(&mut checksum, fragment.baseline);
            add(&mut checksum, fragment.relpos_delta.x);
            add(&mut checksum, fragment.relpos_delta.y);
            checksum = checksum.wrapping_add(fragment.glyphs.as_ref().map_or(0, |glyphs| glyphs.glyphs.len() as i64));
        }
    }
    for piece in &data.inline_box_pieces {
        add(&mut checksum, piece.border_box_rect.x);
        add(&mut checksum, piece.border_box_rect.y);
        add(&mut checksum, piece.border_box_rect.width);
        add(&mut checksum, piece.border_box_rect.height);
        add(&mut checksum, piece.relpos_delta.x);
        add(&mut checksum, piece.relpos_delta.y);
    }
    LineDataFingerprint {
        line_count: data.line_boxes.len(),
        fragment_count,
        piece_count: data.inline_box_pieces.len(),
        geometry_checksum: checksum,
    }
}

/// Digest of the commit-visible rare payloads. The abspos inputs and the
/// inline containing-block rect are deliberately outside it: both are
/// legally written after placement and neither belongs to the fragment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RareDataFingerprint {
    presence: u8,
    scalar_checksum: i64,
}

pub(crate) fn rare_data_fingerprint(rare: &UsedValuesRareData) -> RareDataFingerprint {
    let mut presence = 0u8;
    let mut checksum: i64 = 0;
    let mut mark = |bit: u8, present: bool| {
        if present {
            presence |= 1 << bit;
        }
    };
    if let Some(coordinates) = &rare.table_cell_coordinates {
        mark(0, true);
        checksum = checksum
            .wrapping_add(coordinates.row_index as i64)
            .wrapping_add((coordinates.column_index as i64) << 16)
            .wrapping_add((coordinates.row_span as i64) << 32)
            .wrapping_add((coordinates.column_span as i64) << 48);
    }
    // The SVG payloads are presence-only: resource subtrees (masks, clips,
    // gradients, patterns) are laid out once per consumer, rewriting these
    // fields on records that were placed by the first consumer's pass.
    // Commit emits the last write; the move-into-fragment stage must
    // preserve that, so the shadow pins only their existence here.
    mark(1, rare.computed_svg_path.is_some());
    mark(2, rare.computed_svg_transforms.is_some());
    mark(3, rare.svg_viewport_size.is_some());
    mark(4, rare.grid_layout_data.is_some());
    mark(5, rare.flex_layout_data.is_some());
    mark(6, rare.used_grid_tracks.is_some());
    if let Some(borders) = &rare.override_borders_data {
        mark(7, true);
        checksum = checksum
            .wrapping_add(borders.top.border_data.width.raw_value() as i64)
            .wrapping_add(borders.right.border_data.width.raw_value() as i64)
            .wrapping_add(borders.bottom.border_data.width.raw_value() as i64)
            .wrapping_add(borders.left.border_data.width.raw_value() as i64);
    }
    RareDataFingerprint {
        presence,
        scalar_checksum: checksum,
    }
}

/// An absent lazy cell digests the same as one holding no commit-visible
/// payloads: writes to the excluded fields (abspos inputs, the inline
/// containing-block rect) legally initialize the cell after placement.
pub(crate) fn used_values_shadow_fingerprints(used: &UsedValues) -> (LineDataFingerprint, RareDataFingerprint) {
    let line = used
        .line_data
        .get()
        .map(|cell| line_data_fingerprint(&cell.borrow()))
        .unwrap_or_default();
    let rare = used
        .rare_data
        .get()
        .map(|cell| rare_data_fingerprint(&cell.borrow()))
        .unwrap_or_default();
    (line, rare)
}

/// Immutable result of laying out one box, assembled from the box's sealed
/// used-values cells at its placement. Line data and rare payloads join in a
/// later capture stage.
#[derive(Debug)]
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
    /// Shadow-only digests of the payloads a later stage moves onto the
    /// fragment; None when shadow capture is off or the record has none.
    pub(crate) line_data_fingerprint: Option<LineDataFingerprint>,
    pub(crate) rare_data_fingerprint: Option<RareDataFingerprint>,
    pub(crate) table_cell_coordinates: Option<FfiTableCellCoordinates>,
    pub(crate) override_borders_data: Option<FfiBordersData>,
    pub(crate) children: Vec<FragmentLink>,
}

/// One placement of a fragment. Everything the parent decides about the
/// child lives on the link: the emission offset (the placed offset with the
/// committed delta already folded in) and the inset family.
#[derive(Debug)]
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
) -> FragmentLink {
    let (line_data_fingerprint, rare_data_fingerprint) = if capture_shadow_fingerprints() {
        let (line, rare) = used_values_shadow_fingerprints(used);
        (Some(line), Some(rare))
    } else {
        (None, None)
    };
    let (table_cell_coordinates, override_borders_data) = used
        .rare_data
        .get()
        .map(|cell| {
            let rare = cell.borrow();
            (rare.table_cell_coordinates, rare.override_borders_data)
        })
        .unwrap_or((None, None));
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
            line_data_fingerprint,
            rare_data_fingerprint,
            table_cell_coordinates,
            override_borders_data,
            children,
        }),
        offset: crate::layout::point_add(used.content_offset.get(), used.committed_offset_delta.get()),
        inset_left: used.inset_left.get(),
        inset_right: used.inset_right.get(),
        inset_top: used.inset_top.get(),
        inset_bottom: used.inset_bottom.get(),
        containing_line_box_index,
        is_unplaced_orphan,
    }
}

/// What a committing run hands back alongside ChildLayoutResult: the run
/// root's completed child links plus placements that could not attach to a
/// still-open frame and must ride upward.
#[derive(Debug, Default)]
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
        let link = snapshot_link(node, children, used, false, containing_line_box_index);
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
            inner.root_children.push(snapshot_link(node, Vec::new(), used, true, None));
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
            inner.root_children.push(snapshot_link(node, children, used, true, None));
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
                .push(snapshot_link(node, pending.root_children, used, true, None));
        }
        PendingRunResult {
            root_children: inner.root_children,
            late_attachments: inner.late_attachments,
        }
    }
}

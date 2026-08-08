/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

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
    /// The SVG payload family is copied at snapshot and refreshed by the
    /// run-end drain: SVG layout legally rewrites these record fields after
    /// the owning box was placed, so placement-time capture alone would be
    /// stale. Transforms and viewport stay readable on the record in-run
    /// (they back get-or-compute caches); the path handle is write-only and
    /// moves.
    pub(crate) computed_svg_transforms: Option<crate::layout::FfiSvgComputedTransforms>,
    pub(crate) svg_viewport_size: Option<crate::layout::FfiCssPixelSize>,
    pub(crate) computed_svg_path: Cell<Option<RetainedLayoutHandle>>,
    pub(crate) children: Vec<FragmentLink>,
}

impl Fragment {
    pub(crate) fn carries_svg_path(&self) -> bool {
        let handle = self.computed_svg_path.take();
        let carries = handle.is_some();
        self.computed_svg_path.set(handle);
        carries
    }

    /// Field-wise equality for the run-cache shadow oracle, excluding
    /// children (the oracle recurses through links itself), the move-only
    /// path handle, and the presence of computed SVG transforms: those back
    /// a get-or-compute cache, so whether a record held them at snapshot
    /// depends on what asked during that pass (paint timing), not on layout
    /// state — values compare only when both passes materialized them.
    pub(crate) fn shadow_comparable_state_matches(&self, other: &Fragment) -> bool {
        self.node == other.node
            && self.content_inline_size == other.content_inline_size
            && self.content_block_size == other.content_block_size
            && self.margin_left == other.margin_left
            && self.margin_right == other.margin_right
            && self.margin_top == other.margin_top
            && self.margin_bottom == other.margin_bottom
            && self.border_left == other.border_left
            && self.border_right == other.border_right
            && self.border_top == other.border_top
            && self.border_bottom == other.border_bottom
            && self.padding_left == other.padding_left
            && self.padding_right == other.padding_right
            && self.padding_top == other.padding_top
            && self.padding_bottom == other.padding_bottom
            && self.table_cell_coordinates == other.table_cell_coordinates
            && self.override_borders_data == other.override_borders_data
            && self.line_data.is_some() == other.line_data.is_some()
            && self.grid_layout_data.is_some() == other.grid_layout_data.is_some()
            && self.flex_layout_data.is_some() == other.flex_layout_data.is_some()
            && self.used_grid_tracks.is_some() == other.used_grid_tracks.is_some()
            && match (self.computed_svg_transforms, other.computed_svg_transforms) {
                (Some(own_transforms), Some(other_transforms)) => own_transforms == other_transforms,
                _ => true,
            }
            && self.svg_viewport_size == other.svg_viewport_size
    }
}

/// One placement of a fragment. Everything the parent decides about the
/// child lives on the link: the emission offset (the placed offset with the
/// committed delta already folded in) and the inset family.
#[derive(Clone)]
pub(crate) struct FragmentLink {
    /// Shared so a run-cache entry and every placement handed out on its
    /// hits can carry the same position-independent subtree.
    pub(crate) fragment: std::rc::Rc<Fragment>,
    pub(crate) offset: FfiCssPixelPoint,
    /// The raw placed content offset, without the committed delta folded
    /// into `offset`. Rider hops and drain-time geometry compose raw
    /// offsets, so the placement index is built from this field.
    pub(crate) placement_content_offset: FfiCssPixelPoint,
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
}

/// A box carrying anchor names, placed somewhere in this run's subtree: the
/// shell feeds name matching, and the border-box rect — expressed in
/// effective_birth's content space and travelling like every other carried
/// payload — replaces the emission-time chain walk.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct AnchorCandidate {
    pub(crate) node: crate::layout::node_data::NodeSlotId,
    pub(crate) border_box_rect: PhysicalRect,
    pub(crate) effective_birth: crate::layout::node_data::NodeSlotId,
}

pub(crate) fn translate_pending_abspos_payloads(entry: &mut PendingAbsposChild, offset: FfiCssPixelPoint) {
    entry.static_position_rect = crate::layout::translate_static_position_rect(entry.static_position_rect, offset);
    if let Some(rect) = &mut entry.inline_containing_block_rect {
        rect.x += offset.x;
        rect.y += offset.y;
    }
}

/// A payload born in some box's content space that travels the fragment
/// structure until its consumer drains it: hops rebase it space by space,
/// translating it by each folded content offset and renaming its
/// effective_birth to the box whose space it now sits in.
trait CarriedPayload {
    fn effective_birth(&self) -> crate::layout::node_data::NodeSlotId;
    fn set_effective_birth(&mut self, node: crate::layout::node_data::NodeSlotId);
    fn translate_by(&mut self, offset: FfiCssPixelPoint);
}

impl CarriedPayload for PendingAbsposChild {
    fn effective_birth(&self) -> crate::layout::node_data::NodeSlotId {
        self.effective_birth
    }
    fn set_effective_birth(&mut self, node: crate::layout::node_data::NodeSlotId) {
        self.effective_birth = node;
    }
    fn translate_by(&mut self, offset: FfiCssPixelPoint) {
        translate_pending_abspos_payloads(self, offset);
    }
}

impl CarriedPayload for AnchorCandidate {
    fn effective_birth(&self) -> crate::layout::node_data::NodeSlotId {
        self.effective_birth
    }
    fn set_effective_birth(&mut self, node: crate::layout::node_data::NodeSlotId) {
        self.effective_birth = node;
    }
    fn translate_by(&mut self, offset: FfiCssPixelPoint) {
        self.border_box_rect.x += offset.x;
        self.border_box_rect.y += offset.y;
    }
}

fn rebase_rider_into_containing_block_space_if_born_at<Payload: CarriedPayload>(
    rider: &mut Payload,
    placed_box: crate::layout::node_data::NodeSlotId,
    containing_block: Option<crate::layout::node_data::NodeSlotId>,
    placed_box_content_offset: FfiCssPixelPoint,
) {
    if rider.effective_birth() == placed_box
        && let Some(containing_block) = containing_block
    {
        rider.translate_by(placed_box_content_offset);
        rider.set_effective_birth(containing_block);
    }
}

/// Compose while placed: fold the offsets of boxes this run's coverage has
/// placed, stopping at the run root, the first unplaced box, or the first
/// box placed outside this coverage (a table wrapper on a cell's chain —
/// parked there, composed by the owner later). Presence in the placement
/// index is exactly the owned-and-placed condition the record walk used to
/// test.
fn normalize_escaped_payload_toward_run_root<Payload: CarriedPayload>(
    payload: &mut Payload,
    run_root: crate::layout::node_data::NodeSlotId,
    placed_geometry: &DrainGeometryIndex,
    callbacks: &FfiLayoutFcCallbacks,
) {
    while payload.effective_birth() != run_root {
        let Some(content_offset) = placed_geometry.placed_content_offset(payload.effective_birth()) else {
            break;
        };
        payload.translate_by(content_offset);
        let containing_block = callbacks.containing_block(payload.effective_birth());
        if containing_block.is_invalid() {
            break;
        }
        payload.set_effective_birth(containing_block);
    }
}

/// Geometry a placement recorded for one box: what drain-time walks may
/// read about completed subtrees without touching their records.
#[derive(Clone, Copy)]
pub(crate) struct DrainBoxGeometry {
    pub(crate) content_offset: FfiCssPixelPoint,
    pub(crate) padding_left: CssPixels,
    pub(crate) padding_right: CssPixels,
    pub(crate) padding_top: CssPixels,
    pub(crate) padding_bottom: CssPixels,
    pub(crate) content_inline_size: CssPixels,
    pub(crate) content_block_size: CssPixels,
}

impl DrainBoxGeometry {
    pub(crate) fn from_used_values(used: &UsedValues) -> Self {
        Self {
            content_offset: used.content_offset.get(),
            padding_left: used.padding_left.get(),
            padding_right: used.padding_right.get(),
            padding_top: used.padding_top.get(),
            padding_bottom: used.padding_bottom.get(),
            content_inline_size: used.content_inline_size.get(),
            content_block_size: used.content_block_size.get(),
        }
    }

    pub(crate) fn from_placed_geometry(placement: &crate::layout::PlacedGeometry) -> Self {
        Self {
            content_offset: placement.content_offset,
            padding_left: placement.metrics.padding.left,
            padding_right: placement.metrics.padding.right,
            padding_top: placement.metrics.padding.top,
            padding_bottom: placement.metrics.padding.bottom,
            content_inline_size: placement.metrics.content_inline_size,
            content_block_size: placement.metrics.content_block_size,
        }
    }
}

/// Placement-derived geometry for every box a run's coverage has placed,
/// keyed by slot. A slot is present exactly when its box was placed in
/// this coverage: frame nodes and deposited-but-unplaced roots have no
/// links, so they are exactly the boxes a composing walk must stop at.
#[derive(Default)]
pub(crate) struct DrainGeometryIndex {
    by_slot: std::collections::HashMap<u32, DrainBoxGeometry>,
}

impl DrainGeometryIndex {
    fn collect_links(&mut self, links: &[FragmentLink]) {
        for link in links {
            let fragment = &link.fragment;
            self.by_slot.insert(
                fragment.node.slot_index(),
                DrainBoxGeometry {
                    content_offset: link.placement_content_offset,
                    padding_left: fragment.padding_left,
                    padding_right: fragment.padding_right,
                    padding_top: fragment.padding_top,
                    padding_bottom: fragment.padding_bottom,
                    content_inline_size: fragment.content_inline_size,
                    content_block_size: fragment.content_block_size,
                },
            );
            self.collect_links(&fragment.children);
        }
    }

    /// The drain host never places inside its own builder; its geometry
    /// joins from its own record.
    pub(crate) fn register_host_root(&mut self, node: crate::layout::node_data::NodeSlotId, used: &UsedValues) {
        self.by_slot.insert(node.slot_index(), DrainBoxGeometry::from_used_values(used));
    }

    pub(crate) fn placed_content_offset(&self, node: crate::layout::node_data::NodeSlotId) -> Option<FfiCssPixelPoint> {
        self.by_slot.get(&node.slot_index()).map(|geometry| geometry.content_offset)
    }

    #[track_caller]
    pub(crate) fn geometry_of(&self, node: crate::layout::node_data::NodeSlotId) -> DrainBoxGeometry {
        let caller = std::panic::Location::caller();
        self.by_slot.get(&node.slot_index()).copied().unwrap_or_else(|| {
            panic!(
                "the draining coverage has no placement for slot {} (read at {caller})",
                node.slot_index()
            )
        })
    }
}

fn snapshot_link(
    node: crate::layout::node_data::NodeSlotId,
    children: Vec<FragmentLink>,
    used: &UsedValues,
    containing_line_box_index: Option<usize>,
    emission_offset: FfiCssPixelPoint,
) -> FragmentLink {
    let line_data = used.line_data.get().map(|cell| Box::new(cell.take()));
    let rare_payloads = used.rare_data.get().map(|cell| {
        let mut rare = cell.borrow_mut();
        (
            rare.table_cell_coordinates,
            rare.override_borders_data,
            rare.grid_layout_data.take(),
            rare.flex_layout_data.take(),
            rare.used_grid_tracks.take(),
            rare.abspos_layout_inputs,
            rare.computed_svg_transforms,
            rare.svg_viewport_size,
            rare.computed_svg_path.take(),
        )
    });
    let (
        table_cell_coordinates,
        override_borders_data,
        grid_layout_data,
        flex_layout_data,
        used_grid_tracks,
        abspos_layout_inputs,
        computed_svg_transforms,
        svg_viewport_size,
        computed_svg_path,
    ) = rare_payloads.unwrap_or_default();
    FragmentLink {
        fragment: std::rc::Rc::new(Fragment {
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
            computed_svg_transforms,
            svg_viewport_size,
            computed_svg_path: Cell::new(computed_svg_path),
            children,
        }),
        offset: emission_offset,
        placement_content_offset: used.content_offset.get(),
        inset_left: used.inset_left.get(),
        inset_right: used.inset_right.get(),
        inset_top: used.inset_top.get(),
        inset_bottom: used.inset_bottom.get(),
        containing_line_box_index,
        abspos_layout_inputs,
    }
}

/// What a committing run hands back alongside ChildLayoutResult: the run
/// root's completed child links plus placements that could not attach to a
/// still-open frame and must ride upward. Clones share the fragments and
/// copy the link and escape lists — the shape a run-cache hit reissues.
#[derive(Clone, Default)]
pub(crate) struct PendingRunResult {
    pub(crate) root_children: Vec<FragmentLink>,
    pub(crate) late_attachments: Vec<FragmentLink>,
    /// Absolutely positioned registrations whose targets lie outside the
    /// finished run, escape-normalized so their rects compose only from
    /// records read while the reading run owned them.
    pub(crate) escaped_abspos: Vec<PendingAbsposChild>,
    pub(crate) escaped_anchor_candidates: Vec<AnchorCandidate>,
}

impl PendingRunResult {
    pub(crate) fn is_empty(&self) -> bool {
        self.root_children.is_empty() && self.late_attachments.is_empty()
    }
}

/// Flattens a pass's fragment structure into the slot-keyed index commit
/// emits from. The one-fragment-per-slot invariant is structural, so it
/// holds in every build.
pub(crate) fn fold_commit_index(pending: &PendingRunResult) -> std::collections::HashMap<u32, &FragmentLink> {
    fn flatten<'tree>(
        links: &'tree [FragmentLink],
        flattened: &mut std::collections::HashMap<u32, &'tree FragmentLink>,
    ) {
        for link in links {
            let previous = flattened.insert(link.fragment.node.slot_index(), link);
            assert!(
                previous.is_none(),
                "two fragments claim slot {}",
                link.fragment.node.slot_index()
            );
            flatten(&link.fragment.children, flattened);
        }
    }
    let mut flattened = std::collections::HashMap::new();
    flatten(&pending.root_children, &mut flattened);
    flatten(&pending.late_attachments, &mut flattened);
    flattened
}

struct PendingFrame {
    node: crate::layout::node_data::NodeSlotId,
    children: Vec<FragmentLink>,
    pending_abspos: Vec<PendingAbsposChild>,
    anchor_candidates: Vec<AnchorCandidate>,
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
    /// Set by SVG layout's payload writes; gates the run-end drain that
    /// refreshes fragment SVG payloads from records rewritten after their
    /// box's placement, so non-SVG runs never pay for the walk.
    saw_svg_payload_write: Cell<bool>,
    inner: std::cell::RefCell<RunFragmentBuilderInner>,
}

#[derive(Default)]
struct RunFragmentBuilderInner {
    /// Frames of boxes this run has not placed yet; placing a box removes
    /// its frame, so the map holds only the open containing-block chain.
    frames: std::collections::HashMap<u32, PendingFrame>,
    placed_slots: std::collections::HashSet<u32>,
    deposits: std::collections::HashMap<u32, (crate::layout::node_data::NodeSlotId, PendingRunResult)>,
    /// Records this run creates and never places (inline boxes entered for
    /// their box-model metrics); swept into orphan links so the tree covers
    /// the store exactly.
    unplaced_records: Vec<crate::layout::node_data::NodeSlotId>,
    /// Registrations born at the run root, plus riders that arrived here
    /// through hops; drained by target at run tails and entry sweeps.
    pending_abspos_at_root: Vec<PendingAbsposChild>,
    anchor_candidates_at_root: Vec<AnchorCandidate>,
    root_children: Vec<FragmentLink>,
    late_attachments: Vec<FragmentLink>,
}

/// Rider enumeration covers open frames and the root pool. Riders escaped
/// into a parked deposit are invisible until the deposited child's placement
/// refiles them — every query below runs after the placements it cares
/// about, which is what makes that blind spot safe.
impl RunFragmentBuilderInner {
    fn iter_pending_abspos(&self) -> impl Iterator<Item = &PendingAbsposChild> {
        self.frames
            .values()
            .flat_map(|frame| frame.pending_abspos.iter())
            .chain(self.pending_abspos_at_root.iter())
    }

    fn iter_pending_abspos_mut(&mut self) -> impl Iterator<Item = &mut PendingAbsposChild> {
        self.frames
            .values_mut()
            .flat_map(|frame| frame.pending_abspos.iter_mut())
            .chain(self.pending_abspos_at_root.iter_mut())
    }

    fn iter_anchor_candidates(&self) -> impl Iterator<Item = &AnchorCandidate> {
        self.frames
            .values()
            .flat_map(|frame| frame.anchor_candidates.iter())
            .chain(self.anchor_candidates_at_root.iter())
    }
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
            saw_svg_payload_write: Cell::new(false),
            inner: std::cell::RefCell::new(RunFragmentBuilderInner::default()),
        }
    }

    pub(crate) fn new_entry_accumulator(root_node: crate::layout::node_data::NodeSlotId) -> Self {
        Self {
            root_node,
            root_containing_block_slot: None,
            is_entry_accumulator: true,
            saw_svg_payload_write: Cell::new(false),
            inner: std::cell::RefCell::new(RunFragmentBuilderInner::default()),
        }
    }

    /// Declares a record this run creates without ever placing it.
    pub(crate) fn note_unplaced_record(&self, node: crate::layout::node_data::NodeSlotId) {
        self.inner.borrow_mut().unplaced_records.push(node);
    }

    pub(crate) fn note_svg_payload_write(&self) {
        self.saw_svg_payload_write.set(true);
    }

    pub(crate) fn has_svg_payload_writes(&self) -> bool {
        self.saw_svg_payload_write.get()
    }

    /// Registers an absolutely positioned child born in birth_box's content
    /// space; it rides frames and results until its target's tail drains it.
    pub(crate) fn register_pending_abspos(&self, birth_box: crate::layout::node_data::NodeSlotId, entry: PendingAbsposChild) {
        let mut inner = self.inner.borrow_mut();
        #[cfg(debug_assertions)]
        assert!(
            !inner.placed_slots.contains(&birth_box.slot_index()),
            "an abspos registration named an already-placed birth box"
        );
        if birth_box == self.root_node {
            inner.pending_abspos_at_root.push(entry);
            return;
        }
        match inner.frames.get_mut(&birth_box.slot_index()) {
            Some(frame) => frame.pending_abspos.push(entry),
            None => {
                if self.is_entry_accumulator {
                    inner.pending_abspos_at_root.push(entry);
                } else {
                    inner.frames.insert(
                        birth_box.slot_index(),
                        PendingFrame {
                            node: birth_box,
                            children: Vec::new(),
                            pending_abspos: vec![entry],
                            anchor_candidates: Vec::new(),
                        },
                    );
                }
            }
        }
    }

    /// Rewrites the containing-block info of every registration that has
    /// arrived for a target; a grid stamps its grid-area geometry onto its
    /// contained children at completion, while their entries sit here.
    pub(crate) fn stamp_pending_abspos_containing_blocks_for_target(
        &self,
        target: crate::layout::node_data::NodeSlotId,
        containing_block_info_for_child: impl Fn(crate::layout::node_data::NodeSlotId) -> AbsposContainingBlockInfo,
    ) {
        for entry in &mut self.inner.borrow_mut().pending_abspos_at_root {
            if entry.target == target {
                entry.containing_block_info_override = Some(containing_block_info_for_child(entry.child_box));
            }
        }
    }

    /// Whether any registration anywhere in this builder names an inline
    /// containing block — the gate for collecting first/last-line rects.
    pub(crate) fn any_pending_abspos_has_inline_containing_block(&self) -> bool {
        self.inner
            .borrow()
            .iter_pending_abspos()
            .any(|entry| !entry.inline_containing_block.is_invalid())
    }

    pub(crate) fn any_pending_abspos_names_inline_containing_block(&self, inline_box: crate::layout::node_data::NodeSlotId) -> bool {
        self.inner
            .borrow()
            .iter_pending_abspos()
            .any(|entry| entry.inline_containing_block == inline_box)
    }

    /// Stamps a produced first/last-line rect onto every registration naming
    /// this inline containing block. The rect is in the producing inline
    /// run's containing-block space, which is where every such entry's
    /// effective birth sits when the producing run completes.
    pub(crate) fn stamp_inline_containing_block_rect(
        &self,
        inline_box: crate::layout::node_data::NodeSlotId,
        rect: PhysicalRect,
        expected_space: crate::layout::node_data::NodeSlotId,
    ) {
        for entry in self.inner.borrow_mut().iter_pending_abspos_mut() {
            if entry.inline_containing_block == inline_box {
                debug_assert!(
                    entry.effective_birth == expected_space,
                    "an inline containing block rect met an entry in a different space"
                );
                entry.inline_containing_block_rect = Some(rect);
            }
        }
    }

    /// The shells of every anchor-bearing box placed in this run's subtree
    /// so far, for the C++ name-matching lookup.
    pub(crate) fn anchor_candidate_shells(&self, callbacks: &FfiLayoutFcCallbacks) -> Vec<*mut c_void> {
        self.inner
            .borrow()
            .iter_anchor_candidates()
            .map(|candidate| callbacks.shell(candidate.node))
            .collect()
    }

    pub(crate) fn find_anchor_candidate(
        &self,
        node: crate::layout::node_data::NodeSlotId,
    ) -> Option<(PhysicalRect, crate::layout::node_data::NodeSlotId)> {
        self.inner
            .borrow()
            .iter_anchor_candidates()
            .find(|candidate| candidate.node == node)
            .map(|candidate| (candidate.border_box_rect, candidate.effective_birth))
    }

    pub(crate) fn has_pending_abspos_for_target(&self, target: crate::layout::node_data::NodeSlotId) -> bool {
        self.inner
            .borrow()
            .pending_abspos_at_root
            .iter()
            .any(|entry| entry.target == target)
    }

    /// Takes every registration that has arrived for a target, in document
    /// order. Draining can register more, so callers loop until empty.
    pub(crate) fn take_pending_abspos_for_target(
        &self,
        target: crate::layout::node_data::NodeSlotId,
        callbacks: &FfiLayoutFcCallbacks,
    ) -> Vec<PendingAbsposChild> {
        let mut inner = self.inner.borrow_mut();
        let mut taken: Vec<PendingAbsposChild> = inner
            .pending_abspos_at_root
            .extract_if(.., |entry| entry.target == target)
            .collect();
        taken.sort_by(|left, right| {
            if left.child_box == right.child_box {
                std::cmp::Ordering::Equal
            } else if callbacks.is_before(left.child_box, right.child_box) {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        });
        taken
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn absorb_placement(
        &self,
        node: crate::layout::node_data::NodeSlotId,
        containing_block: Option<crate::layout::node_data::NodeSlotId>,
        used: &UsedValues,
        containing_block_is_already_placed: bool,
        containing_line_box_index: Option<usize>,
        emission_offset: FfiCssPixelPoint,
        own_anchor_candidate_border_box_rect: Option<PhysicalRect>,
    ) {
        let mut inner = self.inner.borrow_mut();
        let slot = node.slot_index();
        assert!(inner.placed_slots.insert(slot), "a box was placed twice in one run");
        let (children, mut carried_late, riding_abspos, riding_anchors) = match inner.frames.remove(&slot) {
            Some(frame) => (frame.children, Vec::new(), frame.pending_abspos, frame.anchor_candidates),
            None => match inner.deposits.remove(&slot) {
                Some((_, pending)) => (
                    pending.root_children,
                    pending.late_attachments,
                    pending.escaped_abspos,
                    pending.escaped_anchor_candidates,
                ),
                None => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            },
        };
        let link = snapshot_link(node, children, used, containing_line_box_index, emission_offset);
        inner.late_attachments.append(&mut carried_late);
        self.attach(&mut inner, link, containing_block, containing_block_is_already_placed);
        let content_offset = used.content_offset.get();
        for mut entry in riding_abspos {
            rebase_rider_into_containing_block_space_if_born_at(&mut entry, node, containing_block, content_offset);
            match inner.frames.get_mut(&entry.effective_birth.slot_index()) {
                Some(frame) => frame.pending_abspos.push(entry),
                None => inner.pending_abspos_at_root.push(entry),
            }
        }
        let own_candidate = own_anchor_candidate_border_box_rect.map(|border_box_rect| AnchorCandidate {
            node,
            border_box_rect,
            effective_birth: containing_block.unwrap_or(node),
        });
        for mut candidate in riding_anchors.into_iter().chain(own_candidate) {
            rebase_rider_into_containing_block_space_if_born_at(&mut candidate, node, containing_block, content_offset);
            match inner.frames.get_mut(&candidate.effective_birth.slot_index()) {
                Some(frame) => frame.anchor_candidates.push(candidate),
                None => inner.anchor_candidates_at_root.push(candidate),
            }
        }
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
        if containing_block == self.root_node || Some(containing_block.slot_index()) == self.root_containing_block_slot {
            inner.root_children.push(link);
            return;
        }
        if let Some(frame) = inner.frames.get_mut(&containing_block.slot_index()) {
            frame.children.push(link);
            return;
        }
        // A frame may only open for a containing block this run will still
        // place; a placed containing block's fragment is sealed wherever it
        // was placed (this run's own past — a drained placement targeting a
        // placed table row — a drain host above a descendant run's table
        // internals, an entry sweep above the whole pass), so the link rides
        // upward instead.
        if self.is_entry_accumulator || containing_block_is_already_placed {
            inner.late_attachments.push(link);
            return;
        }
        inner.frames.insert(
            containing_block.slot_index(),
            PendingFrame {
                node: containing_block,
                children: vec![link],
                pending_abspos: Vec::new(),
                anchor_candidates: Vec::new(),
            },
        );
    }

    /// Every box the builder's current structure has placed, with the
    /// geometry drain-time walks are allowed to read. Built fresh per use:
    /// draining places more boxes, and the escape index must predate the
    /// orphan sweeps (a swept orphan was never placed and must stay a stop).
    pub(crate) fn drain_geometry_index(&self) -> DrainGeometryIndex {
        let inner = self.inner.borrow();
        let mut index = DrainGeometryIndex::default();
        index.collect_links(&inner.root_children);
        index.collect_links(&inner.late_attachments);
        for frame in inner.frames.values() {
            index.collect_links(&frame.children);
        }
        for (_, pending) in inner.deposits.values() {
            index.collect_links(&pending.root_children);
            index.collect_links(&pending.late_attachments);
        }
        index
    }

    /// Closes the run: sweeps never-placed frames and deposits into orphan
    /// links snapshotted from their records, and returns the run root's
    /// completed structure. The builder is empty afterwards.
    pub(crate) fn take_pending_result(&self, records: &RunRecords, callbacks: &FfiLayoutFcCallbacks) -> PendingRunResult {
        let escape_normalization_needed = {
            let inner = self.inner.borrow();
            !inner.pending_abspos_at_root.is_empty()
                || !inner.anchor_candidates_at_root.is_empty()
                || inner
                    .frames
                    .values()
                    .any(|frame| !frame.pending_abspos.is_empty() || !frame.anchor_candidates.is_empty())
        };
        let placed_geometry = escape_normalization_needed.then(|| self.drain_geometry_index());
        let mut inner = self.inner.take();
        let mut escaped_abspos = std::mem::take(&mut inner.pending_abspos_at_root);
        let mut escaped_anchor_candidates = std::mem::take(&mut inner.anchor_candidates_at_root);
        let unplaced_records = std::mem::take(&mut inner.unplaced_records);
        for node in unplaced_records {
            let slot = node.slot_index();
            // A frame or deposit under the same slot is swept below and
            // already carries the record.
            if inner.frames.contains_key(&slot) || inner.deposits.contains_key(&slot) {
                continue;
            }
            let used = records.used_values(node);
            debug_assert!(!used.has_content_offset.get(), "an unplaced record note was placed after all");
            inner
                .root_children
                .push(snapshot_link(node, Vec::new(), &used, None, used.content_offset.get()));
        }
        let frames = std::mem::take(&mut inner.frames);
        for (_, frame) in frames {
            escaped_abspos.extend(frame.pending_abspos);
            escaped_anchor_candidates.extend(frame.anchor_candidates);
            if frame.children.is_empty() {
                continue;
            }
            let used = records.used_values(frame.node);
            inner
                .root_children
                .push(snapshot_link(frame.node, frame.children, &used, None, used.content_offset.get()));
        }
        let deposits = std::mem::take(&mut inner.deposits);
        for (_, (node, pending)) in deposits {
            let used = records.used_values(node);
            inner.late_attachments.extend(pending.late_attachments);
            inner
                .root_children
                .push(snapshot_link(node, pending.root_children, &used, None, used.content_offset.get()));
        }
        if !(escaped_abspos.is_empty() && escaped_anchor_candidates.is_empty()) {
            let placed_geometry = placed_geometry
                .as_ref()
                .expect("the pre-sweep rider peek covers every pool escapes drain from");
            for entry in &mut escaped_abspos {
                debug_assert!(
                    entry.target != self.root_node,
                    "a registration that arrived for this run's root was left undrained"
                );
                normalize_escaped_payload_toward_run_root(entry, self.root_node, placed_geometry, callbacks);
            }
            for candidate in &mut escaped_anchor_candidates {
                normalize_escaped_payload_toward_run_root(candidate, self.root_node, placed_geometry, callbacks);
            }
        }
        if self.saw_svg_payload_write.take() {
            refresh_svg_payloads_from_records(&mut inner.root_children, records);
            refresh_svg_payloads_from_records(&mut inner.late_attachments, records);
        }
        PendingRunResult {
            root_children: inner.root_children,
            late_attachments: inner.late_attachments,
            escaped_abspos,
            escaped_anchor_candidates,
        }
    }
}

/// SVG layout rewrites transform/viewport/path record fields after the owning
/// box was placed (resource content revisited per consumer), so fragments
/// snapshotted at placement can hold stale copies. Refresh every fragment in
/// the finished run from its record: the run just completed, so these are the
/// final values commit must emit.
fn refresh_svg_payloads_from_records(links: &mut [FragmentLink], records: &RunRecords) {
    for link in links {
        // A fragment whose record this run does not own came in through a
        // child-run deposit, and so did its whole subtree: nothing below it
        // can be this run's to refresh, so the walk stops without touching
        // the possibly shared allocation.
        let Some(owned_record) = records.used_values_if_owned(link.fragment.node) else {
            continue;
        };
        let fragment = std::rc::Rc::get_mut(&mut link.fragment)
            .expect("a fragment with an owned record is singly referenced at its run's tail");
        if let Some(rare_cell) = owned_record.rare_data.get() {
            let mut rare = rare_cell.borrow_mut();
            if rare.computed_svg_transforms.is_some() {
                fragment.computed_svg_transforms = rare.computed_svg_transforms;
            }
            if rare.svg_viewport_size.is_some() {
                fragment.svg_viewport_size = rare.svg_viewport_size;
            }
            if let Some(handle) = rare.computed_svg_path.take() {
                fragment.computed_svg_path.set(Some(handle));
            }
        }
        refresh_svg_payloads_from_records(&mut fragment.children, records);
    }
}

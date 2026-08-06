/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

/// An immutable snapshot of one laid-out box, assembled when the formatting
/// context run that created the box's working record ends. Fragments carry no
/// position of their own: a box's offset lives on the containing block's
/// [`FragmentLink`], so an entire subtree is reusable regardless of where the
/// containing block places it.
pub(crate) struct Fragment {
    pub(crate) node: Node,
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
    pub(crate) inset_left: CssPixels,
    pub(crate) inset_right: CssPixels,
    pub(crate) inset_top: CssPixels,
    pub(crate) inset_bottom: CssPixels,
    pub(crate) materialized_from_paintable: bool,
    pub(crate) has_containing_line_box_fragment: bool,
    pub(crate) containing_line_box_fragment: LineBoxFragmentCoordinate,
    pub(crate) links: Vec<FragmentLink>,
}

/// The containing block's record of where one of its boxes went: the flow
/// offset assigned through place_child, the relative-positioning delta and
/// containing-line-box override folded in at freeze, and the fragment
/// itself. The committed offset a paintable receives is
/// `static_offset + relpos_delta`; the two stay separate because
/// out-of-flow propagation translates by flow offsets only, and css-break
/// applies relative positioning after fragmentation.
pub(crate) struct FragmentLink {
    pub(crate) static_offset: FfiCssPixelPoint,
    pub(crate) relpos_delta: FfiCssPixelPoint,
    pub(crate) containing_line_box_index: Option<usize>,
    pub(crate) fragment: std::rc::Rc<Fragment>,
}

impl FragmentLink {
    pub(crate) fn committed_offset(&self) -> FfiCssPixelPoint {
        FfiCssPixelPoint {
            x: self.static_offset.x + self.relpos_delta.x,
            y: self.static_offset.y + self.relpos_delta.y,
        }
    }
}

/// What run_formatting_context returns: the scalar outputs the invoking
/// formatting context consumes directly, and the frozen artifacts of the run.
/// Measurement runs freeze nothing and carry no artifacts.
pub(crate) struct LayoutResult {
    pub(crate) parent_consumed: ChildLayoutResult,
    pub(crate) artifacts: Option<RunArtifacts>,
}

/// One absolutely positioned box waiting for its containing block: born at
/// the registration sites with its static position in the producing box's
/// content space, translated by every placement fold on the way up, and
/// consumed at the containing block's completion once drains flip over.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OofCandidate {
    pub(crate) child_box: Node,
    pub(crate) containing_block: Node,
    pub(crate) inline_containing_block: Node,
    pub(crate) static_position: StaticPositionPoint,
    /// Grid stamps the grid-area geometry at birth; everyone else resolves
    /// the containing block's padding box at the drain.
    pub(crate) containing_block_info: Option<AbsposContainingBlockInfo>,
    /// The first/last-line rect of an inline containing block, stamped by
    /// the inline formatting context and translated with the candidate.
    pub(crate) inline_cb_rect: Option<PhysicalRect>,
    /// True when the containing block is a table cell, row, or row group
    /// whose used geometry the table run finalizes after their own runs:
    /// drains below the table box must skip these.
    pub(crate) waits_for_table_box: bool,
}

/// Border-box rect of an anchor-name-carrying box in its containing block's
/// content space, riding results toward every drain that may resolve an
/// anchor() against it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AnchorRectEntry {
    pub(crate) anchor_box: Node,
    pub(crate) border_box_rect: PhysicalRect,
}

/// The out-of-flow payloads carried on results: translated at placement
/// folds, merged upward, consumed at containing-block drains.
#[derive(Default)]
pub(crate) struct OofCarry {
    pub(crate) candidates: Vec<OofCandidate>,
    pub(crate) anchor_rects: Vec<AnchorRectEntry>,
}

impl OofCarry {
    fn is_empty(&self) -> bool {
        self.candidates.is_empty() && self.anchor_rects.is_empty()
    }

    fn translate(&mut self, offset: FfiCssPixelPoint) {
        for candidate in &mut self.candidates {
            candidate.static_position.offset.inline_offset += offset.x;
            candidate.static_position.offset.block_offset += offset.y;
            if let Some(rect) = &mut candidate.inline_cb_rect {
                rect.x += offset.x;
                rect.y += offset.y;
            }
        }
        for entry in &mut self.anchor_rects {
            entry.border_box_rect.x += offset.x;
            entry.border_box_rect.y += offset.y;
        }
    }

    fn merge(&mut self, mut other: OofCarry) {
        self.candidates.append(&mut other.candidates);
        self.anchor_rects.append(&mut other.anchor_rects);
    }
}

/// What a completed formatting context run hands its invoker: links whose
/// owner is the run root (nested by the parent when it assembles the root
/// fragment at its own freeze), links whose owner froze before the link
/// existed and must keep travelling upward, and the out-of-flow carry in
/// the run root's content space.
pub(crate) struct RunArtifacts {
    pub(crate) root_links: Vec<FragmentLink>,
    pub(crate) escaped_links: Vec<(Node, FragmentLink)>,
    pub(crate) carry: OofCarry,
}

/// Recording scope of one formatting context run: every working record the
/// run created, every placement it assigned, the artifacts of the
/// independent runs it invoked, and the pending out-of-flow carries keyed
/// by the box whose content space their payloads are currently expressed
/// in. Frozen into [`RunArtifacts`] when the run returns.
pub(crate) struct RecorderFrame {
    root: Node,
    created: Vec<Node>,
    placements: Vec<(Node, FfiCssPixelPoint)>,
    placed: HashMap<Node, FfiCssPixelPoint>,
    child_artifacts: HashMap<Node, RunArtifacts>,
    pending_carries: HashMap<Node, OofCarry>,
}

impl LayoutState {
    pub(crate) fn push_recorder_frame(&self, root: Node) {
        if self.is_measurement() {
            return;
        }
        self.recorder_frames.borrow_mut().push(RecorderFrame {
            root,
            created: Vec::new(),
            placements: Vec::new(),
            placed: HashMap::new(),
            child_artifacts: HashMap::new(),
            pending_carries: HashMap::new(),
        });
    }

    pub(crate) fn pop_recorder_frame(&self) -> Option<RecorderFrame> {
        if self.is_measurement() {
            return None;
        }
        let frame = self
            .recorder_frames
            .borrow_mut()
            .pop()
            .expect("a recorder frame pushed for this run must still be active");
        Some(frame)
    }

    pub(crate) fn record_created_used_values(&self, node: Node) {
        if self.is_measurement() {
            return;
        }
        if let Some(frame) = self.recorder_frames.borrow_mut().last_mut() {
            frame.created.push(node);
        }
    }

    pub(crate) fn record_placement(&self, node: Node, offset: FfiCssPixelPoint) {
        if self.is_measurement() {
            return;
        }
        let mut frames = self.recorder_frames.borrow_mut();
        let Some(frame) = frames.last_mut() else {
            return;
        };
        frame.placements.push((node, offset));
        frame.placed.insert(node, offset);
    }

    pub(crate) fn attach_child_artifacts(&self, child_root: Node, artifacts: Option<RunArtifacts>) {
        let Some(mut artifacts) = artifacts else {
            return;
        };
        let mut frames = self.recorder_frames.borrow_mut();
        let frame = frames
            .last_mut()
            .expect("a commit-purpose run always has an invoking recorder frame to attach to");
        let carry = std::mem::take(&mut artifacts.carry);
        if !carry.is_empty() {
            frame.pending_carries.entry(child_root).or_default().merge(carry);
        }
        frame.child_artifacts.insert(child_root, artifacts);
    }

    /// Emits a freshly discovered out-of-flow payload into the pending carry
    /// of the box whose content space it is expressed in.
    pub(crate) fn emit_oof_candidate(&self, space_box: Node, candidate: OofCandidate) {
        if self.is_measurement() {
            return;
        }
        if let Some(frame) = self.recorder_frames.borrow_mut().last_mut() {
            frame.pending_carries.entry(space_box).or_default().candidates.push(candidate);
        }
    }

    pub(crate) fn emit_anchor_rect(&self, space_box: Node, entry: AnchorRectEntry) {
        if self.is_measurement() {
            return;
        }
        if let Some(frame) = self.recorder_frames.borrow_mut().last_mut() {
            frame.pending_carries.entry(space_box).or_default().anchor_rects.push(entry);
        }
    }

    /// Stamps containing block info onto every pending candidate whose
    /// containing block is the given box. A grid container does this at its
    /// run tail, when the grid-area geometry is final, covering absolutely
    /// positioned descendants at any depth for which it is the containing
    /// block — not just its tree children.
    pub(crate) fn stamp_carry_candidates_of_containing_block(
        &self,
        containing_block: Node,
        containing_block_info_for_child: impl Fn(Node) -> AbsposContainingBlockInfo,
    ) {
        if self.is_measurement() {
            return;
        }
        let mut frames = self.recorder_frames.borrow_mut();
        let Some(frame) = frames.last_mut() else {
            return;
        };
        for carry in frame.pending_carries.values_mut() {
            for candidate in &mut carry.candidates {
                if candidate.containing_block == containing_block {
                    candidate.containing_block_info = Some(containing_block_info_for_child(candidate.child_box));
                }
            }
        }
    }

    /// Stamps the first/last-line rect computed for an inline containing
    /// block onto every pending candidate that names it. The rect arrives in
    /// the same content space the candidates were born in — the inline
    /// formatting context's containing block — so it translates with them
    /// from then on.
    pub(crate) fn stamp_inline_cb_rect_on_carry_candidates(&self, space_box: Node, inline_containing_block: Node, rect: PhysicalRect) {
        if self.is_measurement() {
            return;
        }
        if let Some(frame) = self.recorder_frames.borrow_mut().last_mut()
            && let Some(carry) = frame.pending_carries.get_mut(&space_box)
        {
            for candidate in &mut carry.candidates {
                if candidate.inline_containing_block == inline_containing_block {
                    candidate.inline_cb_rect = Some(rect);
                }
            }
        }
    }

    /// Takes the next candidate consumable at `host`'s completion, in tree
    /// order, translated into its containing block's content space. A
    /// candidate is consumable when its containing block is the host itself
    /// (unless the table run still finalizes that box's geometry — a cell
    /// must not drain at its own tail) or a box this frame already placed,
    /// whose geometry froze at that placement. Everything else keeps riding
    /// upward. Candidates can rest under a never-placed space key (an FFI
    /// entry's seeded root); such spaces sit at offset zero in the host, so
    /// their coordinates already agree.
    pub(crate) fn take_next_drainable_oof_candidate(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        host: Node,
    ) -> Option<OofCandidate> {
        let mut frames = self.recorder_frames.borrow_mut();
        let frame = frames.last_mut()?;
        let mut best: Option<(Node, usize, Node)> = None;
        for (space, carry) in &frame.pending_carries {
            for (index, candidate) in carry.candidates.iter().enumerate() {
                let consumable_at_host = candidate.containing_block == host && !candidate.waits_for_table_box;
                let consumable_at_placed_box = frame.placed.contains_key(&candidate.containing_block);
                if !consumable_at_host && !consumable_at_placed_box {
                    continue;
                }
                let is_first = best
                    .as_ref()
                    .is_none_or(|(_, _, best_child)| callbacks.is_before(candidate.child_box, *best_child));
                if is_first {
                    best = Some((*space, index, candidate.child_box));
                }
            }
        }
        let (space, index, _) = best?;
        let mut candidate = frame.pending_carries.get_mut(&space).unwrap().candidates.remove(index);
        let mut origin = FfiCssPixelPoint::default();
        let mut ancestor = candidate.containing_block;
        while ancestor != host && !ancestor.is_invalid() {
            let Some(offset) = frame.placed.get(&ancestor).copied() else {
                break;
            };
            origin.x += offset.x;
            origin.y += offset.y;
            ancestor = callbacks.containing_block(ancestor);
        }
        candidate.static_position.offset.inline_offset -= origin.x;
        candidate.static_position.offset.block_offset -= origin.y;
        if let Some(rect) = &mut candidate.inline_cb_rect {
            rect.x -= origin.x;
            rect.y -= origin.y;
        }
        Some(candidate)
    }

    /// Resolves an anchor's border-box rect for a drain: finds the entry the
    /// carry holds for the anchor box and translates it into the containing
    /// block's content space via the placed-chain origins of its space key
    /// and of the containing block. Acceptable anchors always live inside
    /// the positioned box's containing block subtree, so their entries have
    /// folded into the active frame by consumption time; an absent entry
    /// resolves as no anchor and takes the spec fallback.
    pub(crate) fn carry_anchor_rect_in_containing_block_space(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        anchor_box: Node,
        containing_block: Node,
    ) -> Option<PhysicalRect> {
        let frames = self.recorder_frames.borrow();
        let frame = frames.last()?;
        let (space, entry_rect) = frame.pending_carries.iter().find_map(|(space, carry)| {
            carry
                .anchor_rects
                .iter()
                .find(|entry| entry.anchor_box == anchor_box)
                .map(|entry| (*space, entry.border_box_rect))
        })?;
        let origin_to_frame_root = |node: Node| {
            let mut origin = FfiCssPixelPoint::default();
            let mut ancestor = node;
            while !ancestor.is_invalid() {
                let Some(offset) = frame.placed.get(&ancestor).copied() else {
                    break;
                };
                origin.x += offset.x;
                origin.y += offset.y;
                ancestor = callbacks.containing_block(ancestor);
            }
            origin
        };
        let space_origin = origin_to_frame_root(space);
        let containing_block_origin = origin_to_frame_root(containing_block);
        let mut rect = entry_rect;
        rect.x += space_origin.x - containing_block_origin.x;
        rect.y += space_origin.y - containing_block_origin.y;
        Some(rect)
    }

    pub(crate) fn active_frame_pending_oof_candidate_count(&self) -> usize {
        self.recorder_frames.borrow().last().map_or(0, |frame| {
            frame.pending_carries.values().map(|carry| carry.candidates.len()).sum()
        })
    }

    /// The single translation moment: when a box's offset becomes final, its
    /// pending carry moves into its containing block's content space. If that
    /// containing block was itself already placed (table rows are placed
    /// before the cells that fold into them), the walk keeps translating
    /// through every already-placed ancestor in one bounded pass, so a
    /// payload always rests in a space whose placement is still pending —
    /// or the frame root's.
    pub(crate) fn fold_pending_carry_for_placement(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: Node,
        offset: FfiCssPixelPoint,
    ) {
        if self.is_measurement() {
            return;
        }
        let mut frames = self.recorder_frames.borrow_mut();
        let Some(frame) = frames.last_mut() else {
            return;
        };
        let Some(mut carry) = frame.pending_carries.remove(&node) else {
            return;
        };
        carry.translate(offset);
        let mut space = callbacks.containing_block(node);
        while space != frame.root && !space.is_invalid() {
            let Some(space_offset) = frame.placed.get(&space).copied() else {
                break;
            };
            carry.translate(space_offset);
            space = callbacks.containing_block(space);
        }
        frame.pending_carries.entry(space).or_default().merge(carry);
    }
}

/// Commit's view of the returned trees: one entry per laid-out box, holding
/// the fragment, the committed offset from the owning link, and the line and
/// rare data harvested out of the pass stores once layout has fully finished.
/// Built at the FFI entry from the entry-level artifacts, where top-level
/// links are the pass roots (viewport, subtree root) and escaped links are
/// boxes whose owner fragment froze before they were placed (today only the
/// deferred abspos pass produces those). Commit consumes the harvested data
/// by value: line emission takes the glyph runs and the SVG path handle is
/// transferred exactly once.
pub(crate) struct CommitIndex {
    entries: HashMap<Node, CommitEntry>,
}

pub(crate) struct CommitEntry {
    pub(crate) fragment: std::rc::Rc<Fragment>,
    pub(crate) content_offset: FfiCssPixelPoint,
    pub(crate) containing_line_box_index: Option<usize>,
    pub(crate) line_data: Option<RefCell<LineData>>,
    pub(crate) rare: Option<RefCell<UsedValuesRareData>>,
}

impl CommitIndex {
    pub(crate) fn build(artifacts: &RunArtifacts, state: &LayoutState) -> Self {
        let mut index = CommitIndex {
            entries: HashMap::new(),
        };
        for link in &artifacts.root_links {
            index.insert_link_subtree(link);
        }
        for (_, link) in &artifacts.escaped_links {
            index.insert_link_subtree(link);
        }
        for (node, entry) in &mut index.entries {
            let slot_index = node.slot_index();
            entry.line_data = state.take_line_data_for_commit(slot_index).map(RefCell::new);
            entry.rare = state.take_rare_data_for_commit(slot_index).map(RefCell::new);
        }
        index
    }

    fn insert_link_subtree(&mut self, link: &FragmentLink) {
        self.entries.insert(link.fragment.node, CommitEntry {
            fragment: std::rc::Rc::clone(&link.fragment),
            content_offset: link.committed_offset(),
            containing_line_box_index: link.containing_line_box_index,
            line_data: None,
            rare: None,
        });
        for child_link in &link.fragment.links {
            self.insert_link_subtree(child_link);
        }
    }

    pub(crate) fn entry(&self, node: Node) -> Option<&CommitEntry> {
        self.entries.get(&node)
    }
}

/// Replicates what commit-time offset composition used to do, once per link
/// at freeze: resolve the containing-line-box override against the
/// container's line data (atomic inlines committed at their post-processed
/// fragment offsets, interrupting blocks at their recorded ones), fold the
/// box's own relative-position inset, and fold the relative insets of a
/// fragmented-inline ancestor chain (block-in-inline). Everything read here
/// is same-run state that is final at freeze; offsets materialized from a
/// previous paintable already include all of it.
fn fold_committed_position_into_link(
    state: &LayoutState,
    callbacks: &FfiLayoutFcCallbacks,
    child: Node,
    link: &mut FragmentLink,
) {
    let fragment = &*link.fragment;
    if fragment.materialized_from_paintable {
        return;
    }
    let facts = state.node_facts(callbacks, child);
    if !facts.is_box() || facts.is_fragmented_inline() {
        return;
    }
    if fragment.has_containing_line_box_fragment {
        let containing_block = callbacks.containing_block(child);
        assert!(!containing_block.is_invalid());
        let coordinate = fragment.containing_line_box_fragment;
        if let Some(data) = state.line_data(callbacks.slot_index(containing_block))
            && let Some(line) = data.line_boxes.get(coordinate.line_box_index)
        {
            link.containing_line_box_index = Some(coordinate.line_box_index);
            if let Some(line_fragment) = line.fragments.get(coordinate.fragment_index) {
                let (x, y) = line_fragment.offset();
                link.static_offset = FfiCssPixelPoint { x, y };
            }
        }
    }
    if facts.is_relatively_positioned() {
        link.relpos_delta.x += fragment.inset_left;
        link.relpos_delta.y += fragment.inset_top;
    }
    if facts.is_in_flow() && facts.display().is_block_outside() {
        let chain = state.accumulated_relative_insets_from_inline_ancestor_chain(
            callbacks,
            callbacks.parent(child),
            callbacks.containing_block(child),
        );
        if chain.found_fragmented_inline_node {
            link.relpos_delta.x += chain.offset_x;
            link.relpos_delta.y += chain.offset_y;
        }
    }
}



/// Assembles the frame's records into immutable fragments over the containing
/// block relation. Placements supply link offsets; records the run created
/// but never placed are assembled at their default offsets, mirroring what
/// commit emits for them. Nothing is assembled before the frame ends, which
/// is what absorbs every placement that happens out of tree order: floats
/// batched at the block formatting context tail, atomic inlines placed after
/// line post-processing, table rows before groups before cells, captions
/// placed by the wrapper, markers and legends placed after their siblings.
pub(crate) fn freeze_run_artifacts(
    state: &LayoutState,
    callbacks: &FfiLayoutFcCallbacks,
    frame: RecorderFrame,
) -> RunArtifacts {
    let RecorderFrame {
        root,
        created,
        placements,
        placed: _,
        mut child_artifacts,
        mut pending_carries,
    } = frame;

    let mut placement_offsets = HashMap::<Node, FfiCssPixelPoint>::new();
    for (node, offset) in &placements {
        placement_offsets.insert(*node, *offset);
    }

    let mut links_by_owner = HashMap::<Node, Vec<Node>>::new();
    let placed_then_created = placements
        .iter()
        .map(|(node, _)| *node)
        .chain(created.iter().copied().filter(|node| !placement_offsets.contains_key(node)));
    for node in placed_then_created {
        links_by_owner.entry(callbacks.containing_block(node)).or_default().push(node);
    }

    let mut carry = pending_carries.remove(&root).unwrap_or_default();
    for (_, leftover) in pending_carries.drain() {
        carry.merge(leftover);
    }
    let mut artifacts = RunArtifacts {
        root_links: Vec::new(),
        escaped_links: Vec::new(),
        carry,
    };

    let mut assembly = FragmentAssembly {
        state,
        callbacks,
        placement_offsets: &placement_offsets,
        links_by_owner: &mut links_by_owner,
        child_artifacts: &mut child_artifacts,
        escaped_links: Vec::new(),
    };
    let mut root_links = Vec::new();
    assembly.assemble_links_of(root, &mut root_links);
    artifacts.root_links = root_links;

    let remaining_owners: Vec<Node> = assembly.links_by_owner.keys().copied().collect();
    for owner in remaining_owners {
        let mut links = Vec::new();
        assembly.assemble_links_of(owner, &mut links);
        for link in links {
            assembly.escaped_links.push((owner, link));
        }
    }
    artifacts.escaped_links = std::mem::take(&mut assembly.escaped_links);

    for (_, child) in child_artifacts {
        artifacts.escaped_links.extend(child.escaped_links);
        artifacts.carry.merge(child.carry);
    }
    artifacts
}

struct FragmentAssembly<'freeze, 'pass> {
    state: &'pass LayoutState,
    callbacks: &'pass FfiLayoutFcCallbacks,
    placement_offsets: &'freeze HashMap<Node, FfiCssPixelPoint>,
    links_by_owner: &'freeze mut HashMap<Node, Vec<Node>>,
    child_artifacts: &'freeze mut HashMap<Node, RunArtifacts>,
    escaped_links: Vec<(Node, FragmentLink)>,
}

impl FragmentAssembly<'_, '_> {
    fn assemble_links_of(&mut self, owner: Node, links: &mut Vec<FragmentLink>) {
        let Some(children) = self.links_by_owner.remove(&owner) else {
            return;
        };
        for child in children {
            let static_offset = self.placement_offsets.get(&child).copied().unwrap_or_else(|| {
                self.state
                    .used_values_by_slot(child.slot_index())
                    .expect("every recorded box has a working record until the pass ends")
                    .content_offset
                    .get()
            });
            let mut link = FragmentLink {
                static_offset,
                relpos_delta: FfiCssPixelPoint::default(),
                containing_line_box_index: None,
                fragment: self.assemble(child),
            };
            fold_committed_position_into_link(self.state, self.callbacks, child, &mut link);
            links.push(link);
        }
    }

    fn assemble(&mut self, node: Node) -> std::rc::Rc<Fragment> {
        let mut links = Vec::new();
        if let Some(child) = self.child_artifacts.remove(&node) {
            links = child.root_links;
            for (owner, link) in child.escaped_links {
                self.escaped_links.push((owner, link));
            }
        }
        self.assemble_links_of(node, &mut links);
        let used = self
            .state
            .used_values_by_slot(node.slot_index())
            .expect("every recorded box has a working record until the pass ends");
        std::rc::Rc::new(snapshot_record(node, used, links))
    }
}

fn snapshot_record(node: Node, used: &UsedValues, links: Vec<FragmentLink>) -> Fragment {
    Fragment {
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
        inset_left: used.inset_left.get(),
        inset_right: used.inset_right.get(),
        inset_top: used.inset_top.get(),
        inset_bottom: used.inset_bottom.get(),
        materialized_from_paintable: used.materialized_from_paintable.get(),
        has_containing_line_box_fragment: used.has_containing_line_box_fragment.get(),
        containing_line_box_fragment: used.containing_line_box_fragment.get(),
        links,
    }
}

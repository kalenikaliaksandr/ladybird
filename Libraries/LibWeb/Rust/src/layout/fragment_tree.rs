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

/// What a completed formatting context run hands its invoker: links whose
/// owner is the run root (nested by the parent when it assembles the root
/// fragment at its own freeze), and links whose owner froze before the link
/// existed and must keep travelling upward.
pub(crate) struct RunArtifacts {
    pub(crate) root_links: Vec<FragmentLink>,
    pub(crate) escaped_links: Vec<(Node, FragmentLink)>,
}

/// Recording scope of one formatting context run: every working record the
/// run created, every placement it assigned, and the artifacts of the
/// independent runs it invoked. Frozen into [`RunArtifacts`] when the run
/// returns.
pub(crate) struct RecorderFrame {
    root: Node,
    created: Vec<Node>,
    placements: Vec<(Node, FfiCssPixelPoint)>,
    child_artifacts: HashMap<Node, RunArtifacts>,
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
            child_artifacts: HashMap::new(),
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
        if let Some(frame) = self.recorder_frames.borrow_mut().last_mut() {
            frame.placements.push((node, offset));
        }
    }

    pub(crate) fn attach_child_artifacts(&self, child_root: Node, artifacts: Option<RunArtifacts>) {
        let Some(artifacts) = artifacts else {
            return;
        };
        let mut frames = self.recorder_frames.borrow_mut();
        let frame = frames
            .last_mut()
            .expect("a commit-purpose run always has an invoking recorder frame to attach to");
        frame.child_artifacts.insert(child_root, artifacts);
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

/// Commit-side twin of accumulated_relative_insets_from_inline_ancestor_chain
/// resolving inline-ancestor insets through the returned fragments instead of
/// the working records. Both walks are deleted together when relative
/// positioning folds into link offsets at freeze.
pub(crate) fn accumulated_relative_insets_from_commit_index(
    state: &LayoutState,
    commit_index: &CommitIndex,
    callbacks: &FfiLayoutFcCallbacks,
    first_ancestor: Node,
    stop_at: Node,
) -> InlineAncestorChainRelativeOffset {
    let mut result = InlineAncestorChainRelativeOffset::default();
    let mut ancestor = first_ancestor;
    while !ancestor.is_invalid() && ancestor != stop_at {
        let facts = state.node_facts(callbacks, ancestor);
        if !facts.has_box_model_metrics() {
            break;
        }
        let display = facts.display();
        if !display.is_inline_outside() || !display.is_flow_inside() {
            break;
        }
        result.found_fragmented_inline_node |= facts.is_fragmented_inline();
        if facts.is_relatively_positioned() {
            // An inline that never went through inline layout this pass has
            // no fragment; its committed box model is zeroed, so it
            // contributes no inset.
            if let Some(entry) = commit_index.entry(ancestor) {
                result.offset_x += entry.fragment.inset_left;
                result.offset_y += entry.fragment.inset_top;
            }
        }
        ancestor = callbacks.parent(ancestor);
    }
    result
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
        mut child_artifacts,
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

    let mut artifacts = RunArtifacts {
        root_links: Vec::new(),
        escaped_links: Vec::new(),
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

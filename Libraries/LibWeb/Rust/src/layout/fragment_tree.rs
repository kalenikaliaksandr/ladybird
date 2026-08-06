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
#[expect(dead_code)]
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
/// offset assigned through place_child, the relative-positioning delta folded
/// in at freeze, and the fragment itself. The committed offset a paintable
/// receives is `static_offset + relpos_delta`.
#[expect(dead_code)]
pub(crate) struct FragmentLink {
    pub(crate) static_offset: FfiCssPixelPoint,
    pub(crate) relpos_delta: FfiCssPixelPoint,
    pub(crate) containing_line_box_index: Option<usize>,
    pub(crate) fragment: std::rc::Rc<Fragment>,
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
            links.push(FragmentLink {
                static_offset,
                relpos_delta: FfiCssPixelPoint::default(),
                containing_line_box_index: None,
                fragment: self.assemble(child),
            });
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

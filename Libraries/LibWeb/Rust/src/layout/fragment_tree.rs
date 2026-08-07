/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

/// Immutable structural result of laying out one box, assembled when the box
/// is placed. Field capture deepens in later stages; structure comes first.
#[derive(Debug)]
pub(crate) struct Fragment {
    // Read from the field-capture stage on; identifies the box for the
    // commit pairing.
    #[allow(dead_code)]
    pub(crate) node: crate::layout::node_data::NodeSlotId,
    pub(crate) children: Vec<FragmentLink>,
}

/// One placement of a fragment. Everything the parent decides about the
/// child lives on the link, not on the fragment.
#[derive(Debug)]
pub(crate) struct FragmentLink {
    #[allow(dead_code)]
    pub(crate) node: crate::layout::node_data::NodeSlotId,
    pub(crate) fragment: Box<Fragment>,
    /// The box had a record but was never placed; emitted at the default
    /// offset, exactly as the store-fed commit does today. Read from the
    /// field-capture stage on.
    #[allow(dead_code)]
    pub(crate) is_unplaced_orphan: bool,
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
    /// included — the presence side of the coming shadow diff.
    pub(crate) fn fragment_count(&self) -> usize {
        fn count(links: &[FragmentLink]) -> usize {
            links.iter().map(|link| 1 + count(&link.fragment.children)).sum()
        }
        count(&self.root_children) + count(&self.late_attachments)
    }
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
    inner: std::cell::RefCell<RunFragmentBuilderInner>,
}

#[derive(Default)]
struct RunFragmentBuilderInner {
    frames: std::collections::HashMap<u32, FrameState>,
    deposits: std::collections::HashMap<u32, (crate::layout::node_data::NodeSlotId, PendingRunResult)>,
    root_children: Vec<FragmentLink>,
    late_attachments: Vec<FragmentLink>,
}

impl RunFragmentBuilder {
    pub(crate) fn new(root_node: crate::layout::node_data::NodeSlotId) -> Self {
        Self {
            root_node,
            inner: std::cell::RefCell::new(RunFragmentBuilderInner::default()),
        }
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
    /// deposit into a Fragment and attaches it under its containing block's
    /// frame. This is the one moment payloads may join a link.
    pub(crate) fn absorb_placement(
        &self,
        node: crate::layout::node_data::NodeSlotId,
        containing_block: Option<crate::layout::node_data::NodeSlotId>,
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
        let link = FragmentLink {
            node,
            fragment: Box::new(Fragment { node, children }),
            is_unplaced_orphan: false,
        };
        inner.late_attachments.append(&mut carried_late);
        Self::attach(&mut inner, self.root_node, link, containing_block);
    }

    fn attach(
        inner: &mut RunFragmentBuilderInner,
        root_node: crate::layout::node_data::NodeSlotId,
        link: FragmentLink,
        containing_block: Option<crate::layout::node_data::NodeSlotId>,
    ) {
        let Some(containing_block) = containing_block else {
            inner.root_children.push(link);
            return;
        };
        if containing_block.slot_index() == root_node.slot_index() {
            inner.root_children.push(link);
            return;
        }
        match inner
            .frames
            .entry(containing_block.slot_index())
            .or_insert_with(|| FrameState::Pending {
                node: containing_block,
                children: Vec::new(),
            }) {
            FrameState::Pending { children, .. } => children.push(link),
            // The containing block's fragment is already sealed (a drained
            // placement targeting a placed table row, or a link whose
            // containing block lives outside this run) — ride upward.
            FrameState::Consumed => inner.late_attachments.push(link),
        }
    }

    /// Closes the run: sweeps never-placed frames and deposits into orphan
    /// links, and returns the run root's completed structure. The builder is
    /// empty afterwards; the run holding it returns immediately after.
    pub(crate) fn take_pending_result(&self) -> PendingRunResult {
        let mut inner = self.inner.take();
        let frames = std::mem::take(&mut inner.frames);
        for (_, state) in frames {
            let FrameState::Pending { node, children } = state else {
                continue;
            };
            inner.root_children.push(FragmentLink {
                node,
                fragment: Box::new(Fragment { node, children }),
                is_unplaced_orphan: true,
            });
        }
        let deposits = std::mem::take(&mut inner.deposits);
        for (_, (node, pending)) in deposits {
            inner.late_attachments.extend(pending.late_attachments);
            inner.root_children.push(FragmentLink {
                node,
                fragment: Box::new(Fragment {
                    node,
                    children: pending.root_children,
                }),
                is_unplaced_orphan: true,
            });
        }
        PendingRunResult {
            root_children: inner.root_children,
            late_attachments: inner.late_attachments,
        }
    }
}

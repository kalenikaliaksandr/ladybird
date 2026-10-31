/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The paint-order tree retained from the published recording. Every scope the planner produced is
//! a node with a stable id. A node's child entries name producers of the node's owner and child
//! scopes, each with the size of the output it contributed, so a child's position in the
//! published tape is the sum of the sizes before it and a change inside one scope never touches
//! the entries of another.
//!
//! The tree also says what a recording around the scroll positions may leave out: every entry tells
//! whether it, or anything in it, may be left out or was, and every scope keeps where the output
//! that may be left out lands, so a later recording can keep or leave out a whole scope at once.

use crate::css::style::fast_hash::FastMap;
use crate::layout::node_data::NodeSlotId;
use crate::painting::display_list::commands::SpatialNodeIndex;
use crate::painting::paint_order_plan::{PaintScope, PaintScopeKind, StackingContextPaintPhase};
use crate::painting::record::damage::PaintDamage;
use libgfx_rust::IntRect;
use smallvec::SmallVec;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ScopeId(u32);

const SCOPE_INDEX_BITS: u32 = 24;
const SCOPE_INDEX_MASK: u32 = (1 << SCOPE_INDEX_BITS) - 1;
// A child entry tells scopes from producers by the top bit of its key, which an id therefore
// never sets: a slot whose generation reaches this many retirements is never reused.
const SCOPE_GENERATION_LIMIT: u8 = 127;

impl ScopeId {
    pub(crate) const NONE: Self = Self(u32::MAX);

    fn new(index: usize, generation: u8) -> Self {
        assert!(
            index < SCOPE_INDEX_MASK as usize,
            "paint-order tree exhausted its scope ids"
        );
        assert!(
            (1..=SCOPE_GENERATION_LIMIT).contains(&generation),
            "paint scope generation {generation} is out of range"
        );
        Self(index as u32 | (u32::from(generation) << SCOPE_INDEX_BITS))
    }

    fn index(self) -> usize {
        (self.0 & SCOPE_INDEX_MASK) as usize
    }

    fn generation(self) -> u8 {
        (self.0 >> SCOPE_INDEX_BITS) as u8
    }

    pub(crate) fn is_none(self) -> bool {
        self == Self::NONE
    }
}

impl Default for ScopeId {
    fn default() -> Self {
        Self::NONE
    }
}

impl std::fmt::Debug for ScopeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_none() {
            write!(formatter, "ScopeId(none)")
        } else {
            write!(formatter, "ScopeId({}g{})", self.index(), self.generation())
        }
    }
}

/// One unit of recorded output owned by a row. The order of the draw and hit-test phases
/// mirrors `PaintPhase`, and every kind has the damage bit that makes it record again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub(crate) enum ProducerKind {
    DrawBackground = 0,
    DrawBorder,
    DrawTableCollapsedBorder,
    DrawForeground,
    DrawOutline,
    DrawOverlay,
    HitBackground,
    HitForeground,
    HitOverlay,
    ScrollMetadata,
    ScopePreamble,
    Svg,
    InlinePiece(u32),
    TextFragment(u32),
}

impl ProducerKind {
    const ALL: [Self; 12] = [
        Self::DrawBackground,
        Self::DrawBorder,
        Self::DrawTableCollapsedBorder,
        Self::DrawForeground,
        Self::DrawOutline,
        Self::DrawOverlay,
        Self::HitBackground,
        Self::HitForeground,
        Self::HitOverlay,
        Self::ScrollMetadata,
        Self::ScopePreamble,
        Self::Svg,
    ];

    pub(crate) fn damage(self) -> PaintDamage {
        match self {
            Self::DrawBackground => PaintDamage::DRAW_BACKGROUND,
            Self::DrawBorder => PaintDamage::DRAW_BORDER,
            Self::DrawTableCollapsedBorder => PaintDamage::DRAW_TABLE_COLLAPSED_BORDER,
            Self::DrawForeground => PaintDamage::DRAW_FOREGROUND,
            Self::DrawOutline => PaintDamage::DRAW_OUTLINE,
            Self::DrawOverlay => PaintDamage::DRAW_OVERLAY,
            Self::HitBackground => PaintDamage::HIT_BACKGROUND,
            Self::HitForeground => PaintDamage::HIT_FOREGROUND,
            Self::HitOverlay => PaintDamage::HIT_OVERLAY,
            Self::ScrollMetadata => PaintDamage::SCROLL_METADATA,
            Self::ScopePreamble => PaintDamage::SCOPE_PREAMBLE,
            Self::Svg => PaintDamage::SVG,
            Self::InlinePiece(_) => {
                PaintDamage::DRAW_BACKGROUND | PaintDamage::DRAW_BORDER | PaintDamage::DRAW_FOREGROUND
            }
            Self::TextFragment(_) => PaintDamage::DRAW_FOREGROUND,
        }
    }

    fn from_code(code: u32) -> Self {
        match code & 15 {
            12 => Self::InlinePiece(code >> 4),
            13 => Self::TextFragment(code >> 4),
            kind => Self::ALL[kind as usize],
        }
    }

    pub(super) fn code(self) -> u32 {
        match self {
            Self::InlinePiece(index) | Self::TextFragment(index) => {
                assert!(index < 1 << 27, "inline content exceeds producer key capacity");
                (index << 4) | if matches!(self, Self::InlinePiece(_)) { 12 } else { 13 }
            }
            _ => Self::ALL.iter().position(|kind| *kind == self).unwrap() as u32,
        }
    }
}

/// The output one child contributed to the published recording.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OutputSize {
    pub bytes: u32,
    pub hits: u32,
    pub blocking_wheel_event_regions: u32,
    // Recorded again every frame: scroll-offset-dependent content and resources without
    // retained output. A scope is live when any child is.
    pub live: bool,
    // Recorded again when the window gains or loses focus. A scope reads the window focus when
    // any child does.
    pub reads_window_focus: bool,
    // Left out of a recording around the scroll positions: no bytes, but the hit-test items and
    // blocking wheel regions still count, since hit testing and the tree stay whole-document. A
    // scope is culled when any child is.
    pub culled: bool,
    // Records commands that a recording around the scroll positions may leave out. A scope has
    // cullable output when any child does.
    pub has_cullable_output: bool,
    // Culled while it would have recorded wheel routing: per-box wheel targets, wheel regions,
    // scroll chaining barriers or nested context regions. A scope culled wheel routing when any
    // child did.
    pub culled_wheel_routing: bool,
}

impl OutputSize {
    pub(crate) fn add(&mut self, other: OutputSize) {
        self.bytes += other.bytes;
        self.hits += other.hits;
        self.blocking_wheel_event_regions += other.blocking_wheel_event_regions;
        self.live |= other.live;
        self.reads_window_focus |= other.reads_window_focus;
        self.culled |= other.culled;
        self.has_cullable_output |= other.has_cullable_output;
        self.culled_wheel_routing |= other.culled_wheel_routing;
    }
}

/// Where the commands that a recording around the scroll positions may leave out of a producer, or
/// of every producer in a scope, land: the scroll nodes they record in and, for one of them, what
/// they cover in its content, in device pixels at scroll offset zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CullReach {
    // Bit `index % 64` of the spatial node index of each scroll node. Empty when nothing may be
    // left out.
    scroll_node_bits: u64,
    // One of the scroll nodes and what the commands landing in it cover there. No other scroll node
    // that shares its bit takes any of the commands. A reach that lands in several scroll nodes keeps
    // the bounds of the first one it was given, which for a scope is where its own producers draw.
    bounds_in_scroll_node: Option<(SpatialNodeIndex, IntRect)>,
}

fn scroll_node_bit(scroll_node: SpatialNodeIndex) -> u64 {
    1 << (scroll_node.0 % 64)
}

impl CullReach {
    pub(crate) const NOTHING: Self = Self {
        scroll_node_bits: 0,
        bounds_in_scroll_node: None,
    };

    /// The commands of a producer that recorded without being placed, which may land anywhere.
    pub(crate) const UNPLACED: Self = Self {
        scroll_node_bits: u64::MAX,
        bounds_in_scroll_node: None,
    };

    pub(crate) fn in_scroll_node(scroll_node: SpatialNodeIndex, bounds: IntRect) -> Self {
        Self {
            scroll_node_bits: scroll_node_bit(scroll_node),
            bounds_in_scroll_node: Some((scroll_node, bounds)),
        }
    }

    pub(crate) fn is_nothing(self) -> bool {
        self.scroll_node_bits == 0
    }

    /// The one scroll node the commands land in and what they cover in its content, unless they may land in more.
    pub(crate) fn bounds_in_only_scroll_node(self) -> Option<(SpatialNodeIndex, IntRect)> {
        self.bounds_in_scroll_node
            .filter(|(scroll_node, _)| self.scroll_node_bits == scroll_node_bit(*scroll_node))
    }

    /// Whether the commands may land in any of `scroll_nodes`.
    pub(crate) fn touches(self, scroll_nodes: ScrollNodeSet) -> bool {
        self.scroll_node_bits & scroll_nodes.0 != 0
    }

    /// The one scroll node among `scroll_nodes` that the commands may land in, with what they cover there, unless they
    /// may land in more of them or the bounds of that one are unknown.
    pub(crate) fn bounds_in_only_one_of(self, scroll_nodes: ScrollNodeSet) -> Option<(SpatialNodeIndex, IntRect)> {
        self.bounds_in_scroll_node
            .filter(|(scroll_node, _)| self.scroll_node_bits & scroll_nodes.0 == scroll_node_bit(*scroll_node))
    }

    pub(crate) fn united(self, other: Self) -> Self {
        if self.is_nothing() {
            return other;
        }
        if other.is_nothing() {
            return self;
        }
        // Bounds stay known in a scroll node that the other reach either keeps bounds for too or lands nothing in.
        let kept_beside = |reach: Self, beside: Self| {
            reach
                .bounds_in_scroll_node
                .filter(|(scroll_node, _)| beside.scroll_node_bits & scroll_node_bit(*scroll_node) == 0)
        };
        let bounds_in_scroll_node = match (self.bounds_in_scroll_node, other.bounds_in_scroll_node) {
            (Some((scroll_node, bounds)), Some((other_scroll_node, other_bounds)))
                if scroll_node == other_scroll_node =>
            {
                Some((scroll_node, bounds.united(other_bounds)))
            }
            _ => kept_beside(self, other).or_else(|| kept_beside(other, self)),
        };
        Self {
            scroll_node_bits: self.scroll_node_bits | other.scroll_node_bits,
            bounds_in_scroll_node,
        }
    }

    /// Whether everything `other` reaches, this reaches too.
    #[cfg(test)]
    pub(crate) fn covers(self, other: Self) -> bool {
        if other.is_nothing() {
            return true;
        }
        if self.scroll_node_bits & other.scroll_node_bits != other.scroll_node_bits {
            return false;
        }
        let Some((scroll_node, bounds)) = self.bounds_in_scroll_node else {
            return true;
        };
        match other.bounds_in_scroll_node {
            Some((other_scroll_node, other_bounds)) if other_scroll_node == scroll_node => {
                other_bounds.is_empty() || bounds.contains_rect(other_bounds)
            }
            _ => other.scroll_node_bits & scroll_node_bit(scroll_node) == 0,
        }
    }
}

/// Scroll nodes, kept like the scroll nodes of a `CullReach`, so that whether some content may land in one is a single
/// test. A set holds more than it was given where spatial node indices share a bit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScrollNodeSet(u64);

impl ScrollNodeSet {
    pub(crate) fn insert(&mut self, scroll_node: SpatialNodeIndex) {
        self.0 |= scroll_node_bit(scroll_node);
    }

    pub(crate) fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// What a recording around the scroll positions may leave out of a scope: the reach of its own
/// producers, and of every producer in its subtree, which is the reach of its own producers united
/// with the subtree reach of each child scope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScopeCullSummary {
    pub own_producers: CullReach,
    pub subtree: CullReach,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChildKey {
    Scope(ScopeId),
    Producer(ProducerKind),
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChildEntry {
    key: u32,
    bytes: u32,
    hits: u32,
    meta: u32,
}

const _: () = assert!(std::mem::size_of::<ChildEntry>() == 16);

const SCOPE_KEY_BIT: u32 = 1 << 31;
const META_LIVE: u32 = 1;
const META_READS_WINDOW_FOCUS: u32 = 2;
const META_CULLED: u32 = 1 << 2;
const META_CULLABLE_OUTPUT: u32 = 1 << 3;
const META_CULLED_WHEEL_ROUTING: u32 = 1 << 4;
const META_BLOCKING_REGIONS_SHIFT: u32 = 8;

impl ChildEntry {
    pub(crate) fn scope(id: ScopeId, output: OutputSize) -> Self {
        debug_assert!(!id.is_none());
        Self::with_key(SCOPE_KEY_BIT | id.0, output)
    }

    pub(crate) fn producer(kind: ProducerKind, output: OutputSize) -> Self {
        Self::with_key(kind.code(), output)
    }

    fn with_key(key: u32, output: OutputSize) -> Self {
        let flag = |set: bool, bit: u32| if set { bit } else { 0 };
        let meta = (output.blocking_wheel_event_regions << META_BLOCKING_REGIONS_SHIFT)
            | flag(output.live, META_LIVE)
            | flag(output.reads_window_focus, META_READS_WINDOW_FOCUS)
            | flag(output.culled, META_CULLED)
            | flag(output.has_cullable_output, META_CULLABLE_OUTPUT)
            | flag(output.culled_wheel_routing, META_CULLED_WHEEL_ROUTING);
        Self {
            key,
            bytes: output.bytes,
            hits: output.hits,
            meta,
        }
    }

    pub(crate) fn key(&self) -> ChildKey {
        if self.key & SCOPE_KEY_BIT != 0 {
            ChildKey::Scope(ScopeId(self.key & !SCOPE_KEY_BIT))
        } else {
            ChildKey::Producer(ProducerKind::from_code(self.key))
        }
    }

    pub(crate) fn output(&self) -> OutputSize {
        OutputSize {
            bytes: self.bytes,
            hits: self.hits,
            blocking_wheel_event_regions: self.meta >> META_BLOCKING_REGIONS_SHIFT,
            live: self.meta & META_LIVE != 0,
            reads_window_focus: self.reads_window_focus(),
            culled: self.meta & META_CULLED != 0,
            has_cullable_output: self.has_cullable_output(),
            culled_wheel_routing: self.meta & META_CULLED_WHEEL_ROUTING != 0,
        }
    }

    pub(crate) fn is_live(&self) -> bool {
        self.meta & META_LIVE != 0
    }

    pub(crate) fn reads_window_focus(&self) -> bool {
        self.meta & META_READS_WINDOW_FOCUS != 0
    }

    pub(crate) fn has_cullable_output(&self) -> bool {
        self.meta & META_CULLABLE_OUTPUT != 0
    }
}

impl Default for ChildEntry {
    fn default() -> Self {
        Self::scope(ScopeId(SCOPE_INDEX_MASK), OutputSize::default())
    }
}

const NODE_OCCUPIED: u8 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct ScopeNode {
    owner: NodeSlotId,
    parent: ScopeId,
    children_start: u32,
    children_len: u32,
    // Equal to the tree's assembly stamp while the node is on a damaged path of the current
    // recording, which is what makes an ancestor recurse into it instead of copying it.
    recursion_stamp: u32,
    kind: u8,
    flags: u8,
    generation: u8,
    _padding: u8,
}

const _: () = assert!(std::mem::size_of::<ScopeNode>() == 24);

fn scope_kind_code(kind: PaintScopeKind) -> u8 {
    match kind {
        PaintScopeKind::PaintedAsStackingContext => 0,
        PaintScopeKind::Descendants(phase) => 1 + phase as u8,
    }
}

fn scope_kind_from_code(code: u8) -> PaintScopeKind {
    match code {
        0 => PaintScopeKind::PaintedAsStackingContext,
        1 => PaintScopeKind::Descendants(StackingContextPaintPhase::BackgroundAndBorders),
        2 => PaintScopeKind::Descendants(StackingContextPaintPhase::Floats),
        3 => PaintScopeKind::Descendants(StackingContextPaintPhase::BackgroundAndBordersForInlineLevelAndReplaced),
        4 => PaintScopeKind::Descendants(StackingContextPaintPhase::Foreground),
        _ => unreachable!("unknown paint scope kind code {code}"),
    }
}

pub(crate) const SCOPE_KIND_COUNT: u8 = 1 + StackingContextPaintPhase::COUNT as u8;

#[derive(Clone, Default)]
pub(crate) struct PaintOrderTree {
    nodes: Vec<ScopeNode>,
    free_nodes: Vec<u32>,
    children: Vec<ChildEntry>,
    live_child_entries: u32,
    // The scope of each kind a row owns, indexed by the row's slot. A second scope with the
    // same owner and kind, which the planner produces only for unusual inline-level
    // configurations, lives in the overflow map.
    scopes_by_row: Vec<[ScopeId; SCOPE_KIND_COUNT as usize]>,
    duplicate_scopes: FastMap<(NodeSlotId, u8), SmallVec<[ScopeId; 2]>>,
    // Indexed like the nodes, so a node stays small.
    cull_summaries: Vec<ScopeCullSummary>,
    root: ScopeId,
    root_entry: ChildEntry,
    assembly_stamp: u32,
}

impl PaintOrderTree {
    pub(crate) fn root(&self) -> ScopeId {
        self.root
    }

    pub(crate) fn root_entry(&self) -> ChildEntry {
        self.root_entry
    }

    #[cfg(test)]
    pub(crate) fn scope_count(&self) -> usize {
        self.nodes.iter().filter(|node| node.flags & NODE_OCCUPIED != 0).count()
    }

    #[cfg(test)]
    pub(crate) fn child_entry_capacity(&self) -> usize {
        self.children.len()
    }

    /// Every scope reachable from the root, in tree order, with its child list and cull summary.
    #[cfg(test)]
    pub(crate) fn structure(&self) -> Vec<(ScopeId, PaintScope, Vec<ChildEntry>, ScopeCullSummary)> {
        let mut structure = Vec::new();
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            if id.is_none() {
                continue;
            }
            let children = self.children(id).to_vec();
            for entry in children.iter().rev() {
                if let ChildKey::Scope(child) = entry.key() {
                    stack.push(child);
                }
            }
            structure.push((id, self.scope_of(id), children, self.cull_summary(id)));
        }
        structure
    }

    fn node(&self, id: ScopeId) -> &ScopeNode {
        let node = &self.nodes[id.index()];
        assert_eq!(node.generation, id.generation(), "stale paint scope id {id:?}");
        debug_assert!(node.flags & NODE_OCCUPIED != 0, "freed paint scope id {id:?}");
        node
    }

    fn node_mut(&mut self, id: ScopeId) -> &mut ScopeNode {
        let node = &mut self.nodes[id.index()];
        assert_eq!(node.generation, id.generation(), "stale paint scope id {id:?}");
        debug_assert!(node.flags & NODE_OCCUPIED != 0, "freed paint scope id {id:?}");
        node
    }

    pub(crate) fn scope_of(&self, id: ScopeId) -> PaintScope {
        let node = self.node(id);
        PaintScope {
            owner: node.owner,
            kind: scope_kind_from_code(node.kind),
        }
    }

    #[cfg(test)]
    pub(crate) fn parent(&self, id: ScopeId) -> ScopeId {
        self.node(id).parent
    }

    pub(crate) fn children(&self, id: ScopeId) -> &[ChildEntry] {
        let node = self.node(id);
        let start = node.children_start as usize;
        &self.children[start..start + node.children_len as usize]
    }

    pub(crate) fn cull_summary(&self, id: ScopeId) -> ScopeCullSummary {
        self.node(id);
        self.cull_summaries[id.index()]
    }

    fn scope_in_slot(&self, owner: NodeSlotId, kind_code: u8) -> Option<ScopeId> {
        let id = self.scopes_by_row.get(owner.slot_index() as usize)?[kind_code as usize];
        if id.is_none() {
            return None;
        }
        let node = &self.nodes[id.index()];
        (node.generation == id.generation() && node.flags & NODE_OCCUPIED != 0 && node.owner == owner).then_some(id)
    }

    /// The scope for a plan item under `parent`, if the tree has one. A scope allocated by the
    /// current recording qualifies too; a caller copying from the published recording tells the
    /// two apart by the parent's published child list.
    pub(crate) fn published_scope(&self, scope: PaintScope, parent: ScopeId) -> Option<ScopeId> {
        let kind_code = scope_kind_code(scope.kind);
        if let Some(id) = self.scope_in_slot(scope.owner, kind_code)
            && self.node(id).parent == parent
        {
            return Some(id);
        }
        let ids = self.duplicate_scopes.get(&(scope.owner, kind_code))?;
        ids.iter().copied().find(|id| self.node(*id).parent == parent)
    }

    /// Every scope a row owns, in kind order.
    pub(crate) fn scopes_owned_by(&self, owner: NodeSlotId) -> SmallVec<[ScopeId; 5]> {
        let mut scopes = SmallVec::new();
        for code in 0..SCOPE_KIND_COUNT {
            if let Some(id) = self.scope_in_slot(owner, code) {
                scopes.push(id);
            }
            if !self.duplicate_scopes.is_empty()
                && let Some(ids) = self.duplicate_scopes.get(&(owner, code))
            {
                scopes.extend(ids.iter().copied());
            }
        }
        scopes
    }

    fn register_scope(&mut self, id: ScopeId) {
        let node = self.nodes[id.index()];
        let index = node.owner.slot_index() as usize;
        if self.scopes_by_row.len() <= index {
            self.scopes_by_row
                .resize(index + 1, [ScopeId::NONE; SCOPE_KIND_COUNT as usize]);
        }
        if self.scope_in_slot(node.owner, node.kind).is_some() {
            self.duplicate_scopes
                .entry((node.owner, node.kind))
                .or_default()
                .push(id);
        } else {
            self.scopes_by_row[index][node.kind as usize] = id;
        }
    }

    fn unregister_scope(&mut self, id: ScopeId) {
        let node = self.nodes[id.index()];
        let index = node.owner.slot_index() as usize;
        let key = (node.owner, node.kind);
        if self.scopes_by_row[index][node.kind as usize] == id {
            // A duplicate of the same owner and kind, such as the replacement a scope gets when
            // it moves to another parent, takes over the slot.
            let promoted = self
                .duplicate_scopes
                .get_mut(&key)
                .and_then(|ids| ids.pop())
                .unwrap_or(ScopeId::NONE);
            self.scopes_by_row[index][node.kind as usize] = promoted;
        } else if let Some(ids) = self.duplicate_scopes.get_mut(&key) {
            ids.retain(|candidate| *candidate != id);
        }
        if self.duplicate_scopes.get(&key).is_some_and(|ids| ids.is_empty()) {
            self.duplicate_scopes.remove(&key);
        }
    }

    /// Starts a recording: the assembly stamp advances so no node counts as being on a damaged
    /// path yet.
    pub(crate) fn begin_recording(&mut self) -> u32 {
        self.assembly_stamp = self.assembly_stamp.wrapping_add(1);
        if self.assembly_stamp == 0 {
            for node in &mut self.nodes {
                node.recursion_stamp = 0;
            }
            self.assembly_stamp = 1;
        }
        self.assembly_stamp
    }

    /// Marks the path from a scope to the root as damaged for this recording, stopping at the
    /// first ancestor already marked.
    pub(crate) fn mark_path_to_root_damaged(&mut self, id: ScopeId) {
        let stamp = self.assembly_stamp;
        let mut current = id;
        while !current.is_none() {
            let node = self.node_mut(current);
            if node.recursion_stamp == stamp {
                break;
            }
            node.recursion_stamp = stamp;
            current = node.parent;
        }
    }

    pub(crate) fn is_on_damaged_path(&self, id: ScopeId) -> bool {
        self.node(id).recursion_stamp == self.assembly_stamp
    }

    /// Allocates a scope for this recording and registers it for its owner and kind.
    pub(crate) fn allocate_scope(&mut self, scope: PaintScope, parent: ScopeId) -> ScopeId {
        let node = ScopeNode {
            owner: scope.owner,
            parent,
            children_start: 0,
            children_len: 0,
            recursion_stamp: self.assembly_stamp,
            kind: scope_kind_code(scope.kind),
            flags: NODE_OCCUPIED,
            generation: 1,
            _padding: 0,
        };
        let id = if let Some(index) = self.free_nodes.pop() {
            let slot = &mut self.nodes[index as usize];
            debug_assert_eq!(slot.flags & NODE_OCCUPIED, 0);
            let generation = slot.generation;
            *slot = ScopeNode { generation, ..node };
            self.cull_summaries[index as usize] = ScopeCullSummary::default();
            ScopeId::new(index as usize, generation)
        } else {
            self.nodes.push(node);
            self.cull_summaries.push(ScopeCullSummary::default());
            ScopeId::new(self.nodes.len() - 1, 1)
        };
        self.register_scope(id);
        id
    }

    /// Installs the child list assembled for a scope, whose child scopes have theirs installed
    /// already, with the reach of its own producers. A list of the same length overwrites the
    /// previous one in place; any other length takes new entries at the end of the arena.
    pub(crate) fn set_children(&mut self, id: ScopeId, entries: &[ChildEntry], own_producers: CullReach) {
        let mut subtree = own_producers;
        let mut has_own_cullable_output = false;
        for entry in entries {
            match entry.key() {
                ChildKey::Scope(child) => {
                    let child_subtree = self.cull_summary(child).subtree;
                    debug_assert_eq!(
                        entry.has_cullable_output(),
                        !child_subtree.is_nothing(),
                        "the entry of {child:?} disagrees with its cull summary"
                    );
                    subtree = subtree.united(child_subtree);
                }
                ChildKey::Producer(_) => has_own_cullable_output |= entry.has_cullable_output(),
            }
        }
        debug_assert_eq!(
            has_own_cullable_output,
            !own_producers.is_nothing(),
            "the producer entries of {id:?} disagree with the reach of its own producers"
        );
        self.cull_summaries[id.index()] = ScopeCullSummary { own_producers, subtree };
        let node = *self.node(id);
        let len = u32::try_from(entries.len()).expect("paint-order tree exceeds u32 entries");
        if node.children_len == len {
            let start = node.children_start as usize;
            self.children[start..start + entries.len()].copy_from_slice(entries);
        } else {
            let start = u32::try_from(self.children.len()).expect("paint-order tree exceeds u32 entries");
            self.children.extend_from_slice(entries);
            self.live_child_entries = self.live_child_entries - node.children_len + len;
            let node = self.node_mut(id);
            node.children_start = start;
            node.children_len = len;
        }
    }

    /// Retires a scope no plan item matched, with its subtree. Its ids never match again and
    /// its slots are free for the scopes this recording allocates next.
    pub(crate) fn detach_scope(&mut self, id: ScopeId) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            let node = *self.node(id);
            let start = node.children_start as usize;
            for entry in &self.children[start..start + node.children_len as usize] {
                if let ChildKey::Scope(child) = entry.key() {
                    stack.push(child);
                }
            }
            self.live_child_entries -= node.children_len;
            self.unregister_scope(id);
            let slot = &mut self.nodes[id.index()];
            slot.flags = 0;
            slot.children_len = 0;
            // A retired id never matches again; slots whose generation ran out stay unused.
            if slot.generation < SCOPE_GENERATION_LIMIT {
                slot.generation += 1;
                self.free_nodes.push(id.index() as u32);
            }
        }
    }

    /// Ends the recording with the root's entry, which describes the whole frame, and compacts
    /// the child arena when garbage outgrows the live entries.
    pub(crate) fn finish_recording(&mut self, root: ScopeId, entry: ChildEntry) {
        self.root = root;
        self.root_entry = entry;
        if self.children.len() as u32 - self.live_child_entries > self.live_child_entries {
            self.compact_child_entries();
        }
    }

    // Rebuilds the child arena from the live lists in tree order; nodes keep their ids.
    fn compact_child_entries(&mut self) {
        let mut compacted = Vec::with_capacity(self.live_child_entries as usize);
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            if id.is_none() {
                continue;
            }
            let node = *self.node(id);
            let start = node.children_start as usize;
            let new_start = compacted.len() as u32;
            compacted.extend_from_slice(&self.children[start..start + node.children_len as usize]);
            self.nodes[id.index()].children_start = new_start;
            for entry in compacted[new_start as usize..].iter().rev() {
                if let ChildKey::Scope(child) = entry.key() {
                    stack.push(child);
                }
            }
        }
        self.children = compacted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(index: u32) -> NodeSlotId {
        NodeSlotId::new(index, 1)
    }

    fn output(bytes: u32, hits: u32) -> OutputSize {
        OutputSize {
            bytes,
            hits,
            ..OutputSize::default()
        }
    }

    fn context_scope(owner: NodeSlotId) -> PaintScope {
        PaintScope::stacking_context(owner)
    }

    fn foreground_scope(owner: NodeSlotId) -> PaintScope {
        PaintScope {
            owner,
            kind: PaintScopeKind::Descendants(StackingContextPaintPhase::Foreground),
        }
    }

    // A root context with one foreground scope holding `leaf_count` leaf scopes of one
    // producer each, recorded as the first frame.
    fn record_first_frame(tree: &mut PaintOrderTree, leaf_count: u32) -> (ScopeId, ScopeId, Vec<ScopeId>) {
        tree.begin_recording();
        let root = tree.allocate_scope(context_scope(row(0)), ScopeId::NONE);
        let foreground = tree.allocate_scope(foreground_scope(row(0)), root);
        let mut leaves = Vec::new();
        let mut foreground_children = Vec::new();
        let mut foreground_output = OutputSize::default();
        for index in 0..leaf_count {
            let leaf = tree.allocate_scope(foreground_scope(row(1 + index)), foreground);
            let leaf_output = output(16, 1);
            tree.set_children(
                leaf,
                &[ChildEntry::producer(ProducerKind::DrawForeground, leaf_output)],
                CullReach::NOTHING,
            );
            foreground_children.push(ChildEntry::scope(leaf, leaf_output));
            foreground_output.add(leaf_output);
            leaves.push(leaf);
        }
        tree.set_children(foreground, &foreground_children, CullReach::NOTHING);
        let mut root_output = output(32, 0);
        root_output.add(foreground_output);
        tree.set_children(
            root,
            &[
                ChildEntry::producer(ProducerKind::DrawBackground, output(32, 0)),
                ChildEntry::scope(foreground, foreground_output),
            ],
            CullReach::NOTHING,
        );
        tree.finish_recording(root, ChildEntry::scope(root, root_output));
        (root, foreground, leaves)
    }

    #[test]
    fn scopes_keep_their_ids_and_sizes_add_up() {
        let mut tree = PaintOrderTree::default();
        let (root, foreground, leaves) = record_first_frame(&mut tree, 3);

        assert_eq!(tree.root(), root);
        assert_eq!(tree.root_entry().output(), output(32 + 3 * 16, 3));
        assert_eq!(tree.published_scope(foreground_scope(row(0)), root), Some(foreground));
        assert_eq!(
            tree.published_scope(foreground_scope(row(2)), foreground),
            Some(leaves[1])
        );
        assert_eq!(tree.published_scope(foreground_scope(row(2)), root), None);
        assert_eq!(tree.scopes_owned_by(row(0)).as_slice(), &[root, foreground]);
        assert_eq!(tree.children(foreground).len(), 3);
        assert_eq!(tree.scope_of(leaves[0]), foreground_scope(row(1)));
        assert_eq!(tree.parent(leaves[0]), foreground);
        assert_eq!(tree.scope_count(), 5);
    }

    #[test]
    fn damaged_paths_are_marked_up_to_the_first_marked_ancestor() {
        let mut tree = PaintOrderTree::default();
        let (root, foreground, leaves) = record_first_frame(&mut tree, 2);
        tree.begin_recording();

        tree.mark_path_to_root_damaged(leaves[0]);
        assert!(tree.is_on_damaged_path(leaves[0]));
        assert!(tree.is_on_damaged_path(foreground));
        assert!(tree.is_on_damaged_path(root));
        assert!(!tree.is_on_damaged_path(leaves[1]));

        tree.begin_recording();
        assert!(!tree.is_on_damaged_path(leaves[0]));
        assert!(!tree.is_on_damaged_path(root));
    }

    #[test]
    fn a_recording_replaces_child_lists_and_retires_detached_subtrees_at_once() {
        let mut tree = PaintOrderTree::default();
        let (root, foreground, leaves) = record_first_frame(&mut tree, 2);
        tree.begin_recording();

        let added = tree.allocate_scope(foreground_scope(row(9)), foreground);
        tree.set_children(
            added,
            &[ChildEntry::producer(ProducerKind::DrawForeground, output(8, 0))],
            CullReach::NOTHING,
        );
        let children = [
            ChildEntry::scope(leaves[1], output(16, 1)),
            ChildEntry::scope(added, output(8, 0)),
        ];
        tree.set_children(foreground, &children, CullReach::NOTHING);
        tree.detach_scope(leaves[0]);
        assert_eq!(tree.published_scope(foreground_scope(row(1)), foreground), None);
        assert_eq!(tree.scope_count(), 4);
        tree.finish_recording(root, ChildEntry::scope(root, output(32 + 16 + 8, 1)));

        assert_eq!(tree.children(foreground), &children);
        assert_eq!(tree.root_entry().output(), output(56, 1));
        assert_eq!(tree.published_scope(foreground_scope(row(9)), foreground), Some(added));

        // The retired slot is reused with a new generation, so the old id can never match.
        tree.begin_recording();
        let reused = tree.allocate_scope(foreground_scope(row(7)), foreground);
        assert_eq!(reused.index(), leaves[0].index());
        assert_ne!(reused, leaves[0]);
    }

    #[test]
    fn a_scope_moving_to_another_parent_takes_over_the_slot_its_predecessor_leaves() {
        let mut tree = PaintOrderTree::default();
        let (root, foreground, leaves) = record_first_frame(&mut tree, 2);
        tree.begin_recording();

        // Row 1's foreground scope is listed under the root now; the old one is still there.
        let moved = tree.allocate_scope(foreground_scope(row(1)), root);
        assert_eq!(tree.published_scope(foreground_scope(row(1)), root), Some(moved));
        assert_eq!(
            tree.published_scope(foreground_scope(row(1)), foreground),
            Some(leaves[0])
        );
        assert_eq!(tree.scopes_owned_by(row(1)).len(), 2);

        tree.detach_scope(leaves[0]);
        assert_eq!(tree.published_scope(foreground_scope(row(1)), root), Some(moved));
        assert_eq!(tree.scopes_owned_by(row(1)).as_slice(), &[moved]);
        assert!(tree.duplicate_scopes.is_empty());
    }

    #[test]
    fn a_slot_retired_many_times_keeps_its_ids_distinguishable_from_producers() {
        let mut tree = PaintOrderTree::default();
        let (root, foreground, _) = record_first_frame(&mut tree, 1);
        for round in 0..300u32 {
            tree.begin_recording();
            for child in tree.children(foreground).to_vec() {
                if let ChildKey::Scope(previous) = child.key() {
                    tree.detach_scope(previous);
                }
            }
            let added = tree.allocate_scope(foreground_scope(row(100 + round)), foreground);
            tree.set_children(
                added,
                &[ChildEntry::producer(ProducerKind::DrawForeground, output(8, 0))],
                CullReach::NOTHING,
            );
            let entry = ChildEntry::scope(added, output(8, 0));
            assert_eq!(entry.key(), ChildKey::Scope(added));
            tree.set_children(foreground, &[entry], CullReach::NOTHING);
            tree.finish_recording(root, ChildEntry::scope(root, output(40, 0)));
            assert_eq!(
                tree.published_scope(foreground_scope(row(100 + round)), foreground),
                Some(added)
            );
        }
        assert_eq!(tree.scope_count(), 3);
    }

    #[test]
    fn same_length_child_lists_are_overwritten_in_place() {
        let mut tree = PaintOrderTree::default();
        let (root, foreground, leaves) = record_first_frame(&mut tree, 2);
        let capacity = tree.child_entry_capacity();
        tree.begin_recording();

        let grown = output(24, 1);
        tree.set_children(
            leaves[0],
            &[ChildEntry::producer(ProducerKind::DrawForeground, grown)],
            CullReach::NOTHING,
        );
        let children = [
            ChildEntry::scope(leaves[0], grown),
            ChildEntry::scope(leaves[1], output(16, 1)),
        ];
        tree.set_children(foreground, &children, CullReach::NOTHING);
        tree.finish_recording(root, ChildEntry::scope(root, output(32 + 24 + 16, 2)));

        assert_eq!(tree.children(foreground), &children);
        assert_eq!(tree.child_entry_capacity(), capacity);
    }

    #[test]
    fn the_child_arena_stays_bounded_under_staggered_updates() {
        let mut tree = PaintOrderTree::default();
        let (root, foreground, leaves) = record_first_frame(&mut tree, 8);
        let mut peak = 0;
        for round in 0..4000u32 {
            tree.begin_recording();
            let leaf = leaves[(round % 8) as usize];
            let leaf_output = output(16 + round % 3, 1);
            // Every other round the leaf gains or loses a producer, so its list changes length.
            let leaf_children: Vec<ChildEntry> = if round % 2 == 0 {
                vec![ChildEntry::producer(ProducerKind::DrawForeground, leaf_output)]
            } else {
                vec![
                    ChildEntry::producer(ProducerKind::DrawBackground, output(0, 0)),
                    ChildEntry::producer(ProducerKind::DrawForeground, leaf_output),
                ]
            };
            tree.set_children(leaf, &leaf_children, CullReach::NOTHING);
            let children: Vec<ChildEntry> = tree
                .children(foreground)
                .iter()
                .map(|entry| match entry.key() {
                    ChildKey::Scope(id) if id == leaf => ChildEntry::scope(leaf, leaf_output),
                    _ => *entry,
                })
                .collect();
            tree.set_children(foreground, &children, CullReach::NOTHING);
            let mut total = output(32, 0);
            for entry in &children {
                total.add(entry.output());
            }
            tree.finish_recording(root, ChildEntry::scope(root, total));
            peak = peak.max(tree.child_entry_capacity());
            assert_eq!(tree.root_entry().output().hits, 8);
        }
        let live = 2 + 8 + 2 * 8;
        assert!(
            peak <= live * 2 + 16,
            "child arena grew to {peak} entries for {live} live ones"
        );
        assert_eq!(tree.scope_count(), 10);
    }

    fn cullable_output(bytes: u32) -> OutputSize {
        OutputSize {
            has_cullable_output: true,
            ..output(bytes, 0)
        }
    }

    fn in_scroll_node(scroll_node: u32, bounds: IntRect) -> CullReach {
        CullReach::in_scroll_node(SpatialNodeIndex(scroll_node), bounds)
    }

    #[test]
    fn every_cull_flag_survives_a_child_entry_on_its_own_and_scopes_unite_them() {
        let flags = [
            OutputSize {
                live: true,
                ..OutputSize::default()
            },
            OutputSize {
                reads_window_focus: true,
                ..OutputSize::default()
            },
            OutputSize {
                culled: true,
                ..OutputSize::default()
            },
            OutputSize {
                has_cullable_output: true,
                ..OutputSize::default()
            },
            OutputSize {
                culled_wheel_routing: true,
                ..OutputSize::default()
            },
        ];
        let mut united = OutputSize::default();
        for flag in flags {
            let with_counts = OutputSize {
                hits: 3,
                blocking_wheel_event_regions: 2,
                ..flag
            };
            assert_eq!(
                ChildEntry::producer(ProducerKind::ScrollMetadata, with_counts).output(),
                with_counts
            );
            united.add(flag);
        }
        let all_flags = OutputSize {
            bytes: 0,
            hits: 0,
            blocking_wheel_event_regions: 0,
            live: true,
            reads_window_focus: true,
            culled: true,
            has_cullable_output: true,
            culled_wheel_routing: true,
        };
        assert_eq!(united, all_flags);
        assert_eq!(ChildEntry::scope(ScopeId::new(5, 1), all_flags).output(), all_flags);
    }

    #[test]
    fn a_culled_entry_keeps_counting_its_hit_test_items_and_blocking_wheel_regions() {
        let culled = OutputSize {
            bytes: 0,
            hits: 3,
            blocking_wheel_event_regions: 2,
            culled: true,
            has_cullable_output: true,
            culled_wheel_routing: true,
            ..OutputSize::default()
        };
        let entry = ChildEntry::producer(ProducerKind::DrawForeground, culled);
        assert!(entry.has_cullable_output());
        let mut scope_output = output(40, 1);
        scope_output.add(entry.output());
        assert_eq!(
            scope_output,
            OutputSize {
                bytes: 40,
                hits: 4,
                ..culled
            }
        );
    }

    #[test]
    fn cull_reaches_keep_the_bounds_of_their_first_scroll_node_beside_others() {
        let near = in_scroll_node(1, IntRect::new(0, 0, 10, 10));
        let far = in_scroll_node(1, IntRect::new(0, 100, 10, 10));
        let nested = in_scroll_node(2, IntRect::new(0, 0, 10, 10));
        assert!(CullReach::NOTHING.is_nothing() && CullReach::default().is_nothing());
        assert_eq!(CullReach::NOTHING.united(near), near);
        assert_eq!(near.united(CullReach::NOTHING), near);
        assert_eq!(near.united(far), in_scroll_node(1, IntRect::new(0, 0, 10, 110)));

        let in_two_scroll_nodes = near.united(nested);
        assert_eq!(
            in_two_scroll_nodes.bounds_in_scroll_node,
            Some((SpatialNodeIndex(1), IntRect::new(0, 0, 10, 10)))
        );
        assert_eq!(
            in_two_scroll_nodes.united(far).bounds_in_scroll_node,
            Some((SpatialNodeIndex(1), IntRect::new(0, 0, 10, 110)))
        );
        assert_eq!(
            nested.united(near).bounds_in_scroll_node,
            Some((SpatialNodeIndex(2), IntRect::new(0, 0, 10, 10)))
        );
        assert!(in_two_scroll_nodes.covers(near) && in_two_scroll_nodes.covers(nested));
        assert!(!near.covers(in_two_scroll_nodes) && !nested.covers(near) && !near.covers(far));
        assert!(!in_two_scroll_nodes.covers(far) && !in_two_scroll_nodes.covers(nested.united(far)));
        assert!(!in_two_scroll_nodes.covers(in_scroll_node(3, IntRect::new(0, 0, 1, 1))));

        // A scroll node that shares its bit with another keeps no bounds once both are reached.
        let sharing_the_bit_of_near = in_scroll_node(65, IntRect::new(0, 0, 10, 10));
        assert_eq!(near.united(sharing_the_bit_of_near).bounds_in_scroll_node, None);

        assert_eq!(in_two_scroll_nodes.united(CullReach::UNPLACED), CullReach::UNPLACED);
        assert!(CullReach::UNPLACED.covers(in_two_scroll_nodes));
        assert!(!in_two_scroll_nodes.covers(CullReach::UNPLACED));
    }

    #[test]
    fn a_scope_summary_unites_its_own_producers_with_the_subtrees_of_its_child_scopes() {
        let mut tree = PaintOrderTree::default();
        tree.begin_recording();
        let root = tree.allocate_scope(context_scope(row(0)), ScopeId::NONE);
        let foreground = tree.allocate_scope(foreground_scope(row(0)), root);
        let near_reach = in_scroll_node(1, IntRect::new(0, 0, 100, 100));
        let far_reach = in_scroll_node(1, IntRect::new(0, 5000, 100, 100));
        let nested_reach = in_scroll_node(2, IntRect::new(0, 0, 50, 50));
        let mut leaf_entries = Vec::new();
        for (index, reach) in [near_reach, far_reach, nested_reach].into_iter().enumerate() {
            let leaf = tree.allocate_scope(foreground_scope(row(1 + index as u32)), foreground);
            tree.set_children(
                leaf,
                &[
                    ChildEntry::producer(ProducerKind::HitForeground, output(0, 1)),
                    ChildEntry::producer(ProducerKind::DrawForeground, cullable_output(16)),
                ],
                reach,
            );
            let mut leaf_output = output(0, 1);
            leaf_output.add(cullable_output(16));
            leaf_entries.push(ChildEntry::scope(leaf, leaf_output));
            assert_eq!(
                tree.cull_summary(leaf),
                ScopeCullSummary {
                    own_producers: reach,
                    subtree: reach,
                }
            );
        }
        let record_root = |tree: &mut PaintOrderTree, leaf_entries: &[ChildEntry]| {
            tree.set_children(foreground, leaf_entries, CullReach::NOTHING);
            let mut foreground_output = OutputSize::default();
            for entry in leaf_entries {
                foreground_output.add(entry.output());
            }
            let mut root_output = output(32, 0);
            root_output.add(foreground_output);
            tree.set_children(
                root,
                &[
                    ChildEntry::producer(ProducerKind::DrawBackground, output(32, 0)),
                    ChildEntry::scope(foreground, foreground_output),
                ],
                CullReach::NOTHING,
            );
            tree.finish_recording(root, ChildEntry::scope(root, root_output));
        };
        record_root(&mut tree, &leaf_entries);

        let in_root_scroll_node = in_scroll_node(1, IntRect::new(0, 0, 100, 5100));
        let everywhere = in_root_scroll_node.united(nested_reach);
        assert_eq!(
            tree.cull_summary(foreground),
            ScopeCullSummary {
                own_producers: CullReach::NOTHING,
                subtree: everywhere,
            }
        );
        assert_eq!(tree.cull_summary(root).subtree, everywhere);
        assert!(tree.root_entry().has_cullable_output());

        // Retiring the nested scope leaves the root scroll node only, and a scope that takes its
        // slot starts out with nothing.
        tree.begin_recording();
        let ChildKey::Scope(nested) = leaf_entries[2].key() else {
            unreachable!()
        };
        tree.detach_scope(nested);
        record_root(&mut tree, &leaf_entries[..2]);
        assert_eq!(tree.cull_summary(foreground).subtree, in_root_scroll_node);
        assert_eq!(tree.cull_summary(root).subtree, in_root_scroll_node);
        let reused = tree.allocate_scope(foreground_scope(row(9)), foreground);
        assert_eq!(reused.index(), nested.index());
        assert_eq!(tree.cull_summary(reused), ScopeCullSummary::default());
    }
}

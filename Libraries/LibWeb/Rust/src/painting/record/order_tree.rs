/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! What a recording keeps of the paint order it recorded, for the next recording to copy clean output from.
//!
//! The planner's scopes nest to the depth of the document, and keeping every one of them costs more than the display
//! list they describe. So only some scopes are kept as nodes of their own: the ones a recording walks, because they
//! are too tall to be recorded in one piece, scroll, or may paint anywhere. The plan of a walked scope is cut into
//! runs of consecutive items, each recorded or copied whole, with the walked child scopes between them. A node holds
//! the size of what each of its parts recorded, so a part's position in the tape is the sum of the sizes before it,
//! and a recording shares every node it copied unchanged with the recording it copied from.

use crate::css::css_pixels::CssPixelPoint;
use crate::painting::paint_order_plan::{PaintOrderItem, PaintScope};
use crate::painting::record::damage::PaintDamage;
use std::ops::Range;
use std::sync::Arc;

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
}

/// The output one part of a recording contributed to it.
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
}

impl OutputSize {
    pub(crate) fn add(&mut self, other: OutputSize) {
        self.bytes += other.bytes;
        self.hits += other.hits;
        self.blocking_wheel_event_regions += other.blocking_wheel_event_regions;
        self.live |= other.live;
        self.reads_window_focus |= other.reads_window_focus;
    }
}

/// Where a row's boxes are, as far as an ancestor can move them without anything pushing damage for the row: the
/// origin of its first fragment, and for a row in several fragments a digest of where the later ones are, which a
/// change of the columns they sit in moves on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowPlacement {
    pub origin: CssPixelPoint,
    pub later_fragments: u64,
}

/// A part of a walked scope's plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Segment {
    /// Consecutive items recorded as one piece: producers of the scope's owner, and child scopes with everything
    /// below them.
    Run(Range<u32>),
    /// The item that is a child scope walked on its own.
    Walked(u32),
}

/// The plan of a walked scope with where it is cut.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SegmentedPlan {
    pub active: bool,
    pub items: Vec<PaintOrderItem>,
    pub segments: Vec<Segment>,
}

impl SegmentedPlan {
    pub(crate) fn items_of(&self, run: &Range<u32>) -> &[PaintOrderItem] {
        &self.items[run.start as usize..run.end as usize]
    }

    pub(crate) fn walked_scope(&self, item: u32) -> PaintScope {
        match self.items[item as usize] {
            PaintOrderItem::Scope(scope) => scope,
            PaintOrderItem::Producer(_) => unreachable!("a walked segment names a child scope"),
        }
    }
}

/// What a run recorded, and where the owners of its child scopes were when it did: a child that an ancestor moved
/// since records again though nothing pushed damage for it.
#[derive(Debug)]
pub(crate) struct RecordedRun {
    pub output: OutputSize,
    pub child_scope_placements: Box<[RowPlacement]>,
}

#[derive(Debug)]
pub(crate) enum RecordedSegment {
    Run(RecordedRun),
    Walked(Arc<RecordedScope>),
}

impl RecordedSegment {
    pub(crate) fn output(&self) -> OutputSize {
        match self {
            Self::Run(run) => run.output,
            Self::Walked(scope) => scope.output,
        }
    }
}

/// A walked scope as one recording recorded it.
#[derive(Debug)]
pub(crate) struct RecordedScope {
    pub scope: PaintScope,
    pub plan: Arc<SegmentedPlan>,
    /// One for each segment of the plan.
    pub segments: Vec<RecordedSegment>,
    pub output: OutputSize,
    /// Where the scope's owner was, which the producers of the owner in the scope's runs were recorded against.
    pub owner_placement: RowPlacement,
}

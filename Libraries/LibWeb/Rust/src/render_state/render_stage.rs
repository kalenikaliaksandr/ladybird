/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The work that follows a document's layout before what it renders is read, as stages in the order they run: each
//! stage reads what the stages before it leave. A write to the render state leaves the stage whose inputs it changes
//! stale, and every stage after it, and a host brings the stages up to the one it reads through a single driver.

use super::ArenaChange;
use crate::painting::host::FfiVisualContextTreeInputs;
use std::cell::Cell;

/// A stage of the work that follows a document's layout, in the order the stages run.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RenderStage {
    /// The scrollable overflow is measured, the scroll offsets it moved out of range are clamped into it, and the root
    /// background and the sticky constraints it moved are settled.
    ScrollPositions,
    /// The image map areas and the SVG paint resources the rows name are published.
    PaintProperties,
    /// The visual context tree is built or updated.
    VisualContexts,
    /// The scroll state snapshot is read from the scroll offsets.
    ScrollSnapshot,
    /// Every stage is current.
    Current,
}

impl RenderStage {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::ScrollPositions => Self::PaintProperties,
            Self::PaintProperties => Self::VisualContexts,
            Self::VisualContexts => Self::ScrollSnapshot,
            Self::ScrollSnapshot | Self::Current => Self::Current,
        }
    }
}

/// Which stages of a host's document are stale.
pub(crate) struct RenderPipeline {
    /// The first stage left stale, which leaves every stage after it stale too.
    first_stale: Cell<RenderStage>,
    /// How many writes that change the visual context tree the host queued, and no other stage's inputs.
    visual_context_tree_writes: Cell<u64>,
    /// The inputs of the visual context tree as the host last had them.
    visual_context_tree_inputs: Cell<Option<FfiVisualContextTreeInputs>>,
}

impl Default for RenderPipeline {
    fn default() -> Self {
        Self {
            first_stale: Cell::new(RenderStage::ScrollPositions),
            visual_context_tree_writes: Cell::new(0),
            visual_context_tree_inputs: Cell::new(None),
        }
    }
}

impl RenderPipeline {
    pub(crate) fn first_stale(&self) -> RenderStage {
        self.first_stale.get()
    }

    /// Leaves `stage` stale, and every stage after it.
    pub(crate) fn note_stale(&self, stage: RenderStage) {
        if stage < self.first_stale.get() {
            self.first_stale.set(stage);
        }
    }

    /// Notes what `change`, a write the host queued, leaves stale.
    pub(crate) fn note_write(&self, change: &ArenaChange) {
        let stage = change.first_stale_stage();
        if stage == RenderStage::VisualContexts {
            self.visual_context_tree_writes
                .set(self.visual_context_tree_writes.get() + 1);
        }
        self.note_stale(stage);
    }

    pub(crate) fn visual_context_tree_writes(&self) -> u64 {
        self.visual_context_tree_writes.get()
    }

    /// Notes the inputs of the visual context tree as the host has them now, which leave the tree stale where they moved:
    /// the device pixel ratio and the visual viewport are the page's, and reach the render state only so.
    pub(crate) fn note_visual_context_tree_inputs(&self, inputs: FfiVisualContextTreeInputs) {
        let moved = self.visual_context_tree_inputs.get().is_none_or(|noted| {
            noted.device_pixels_per_css_pixel != inputs.device_pixels_per_css_pixel
                || noted.visual_viewport_offset_x != inputs.visual_viewport_offset_x
                || noted.visual_viewport_offset_y != inputs.visual_viewport_offset_y
                || noted.visual_viewport_scale != inputs.visual_viewport_scale
        });
        if moved {
            self.visual_context_tree_inputs.set(Some(inputs));
            self.note_stale(RenderStage::VisualContexts);
        }
    }

    /// Takes the stale stages up to `target` to run, as the first of them, and leaves them current: a write made while
    /// they run leaves the stage it changes the inputs of stale again, for the next time a host brings them up, but for
    /// a stage that runs after it (see [`Self::begin_stage`]).
    pub(crate) fn take_stale_stages_up_to(&self, target: RenderStage) -> Option<RenderStage> {
        let first_stale = self.first_stale.get();
        if first_stale > target {
            return None;
        }
        self.first_stale.set(target.next());
        Some(first_stale)
    }

    /// Notes that `stage` begins to run as the stages are brought up to `target`: a write an earlier stage made that
    /// left it stale is one it reads.
    pub(crate) fn begin_stage(&self, stage: RenderStage, target: RenderStage) {
        if self.first_stale.get() == stage {
            self.first_stale.set(target.next());
        }
    }
}

impl ArenaChange {
    /// The first stage of the work after layout that the change leaves stale (see [`RenderStage`]), or none.
    pub(crate) fn first_stale_stage(&self) -> RenderStage {
        match self {
            Self::Layout(change) => change.first_stale_stage(),
            Self::Paint(change) => change.first_stale_stage(),
            Self::Style(change) if change.only_keeps_records_alive() => RenderStage::Current,
            Self::Style(_)
            | Self::Engine(_)
            | Self::Rule(_)
            | Self::DetachForRemoval(_)
            | Self::ReinheritAnonymousDescendants(_)
            | Self::RemoveBox(_) => RenderStage::ScrollPositions,
        }
    }
}

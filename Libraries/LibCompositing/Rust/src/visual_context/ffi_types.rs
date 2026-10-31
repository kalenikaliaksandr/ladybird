/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The plain FFI types the visual context tree hands across the C++ boundary on both sides of the
//! compositor process boundary.

use crate::display_list::commands::{OptionalF32, SpatialNodeIndex};
use libgfx_rust::{FloatMatrix4x4, FloatPoint, FloatRect, FloatSize, IntRect};

/// A side of an interest rect this far from the origin is unbounded: nothing beyond it is left out.
pub const UNBOUNDED_INTEREST_EDGE: i32 = 1 << 24;

/// A scroll node of the display list a compositor shows, as a frame it presents shows the node. Offsets and rects are in
/// device pixels, rects of the node's content at scroll offset zero.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct FfiRecordingRefreshScrollNode {
    /// The node as the compositor and WebContent name it.
    pub scrollable_node_id: i64,
    pub scroll_node_kind: u8,
    pub pseudo_element_type: u8,
    /// The node's own spatial node in the display list's visual context tree.
    pub scroll_node_index: SpatialNodeIndex,
    pub scrollport: IntRect,
    pub scroll_offset: FloatPoint,
    pub minimum_scroll_offset: FloatPoint,
    pub maximum_scroll_offset: FloatPoint,
    /// Whether any of the node shows, and what of its content does.
    pub shows: bool,
    pub visible_content_rect: IntRect,
    /// Whether the display list records only part of the node's content, and which part.
    pub records_part: bool,
    pub recorded_content_rect: IntRect,
    /// Whether a smooth scroll the compositor runs moves the node, and where it takes the node by the time a recording
    /// asked for now could arrive.
    pub has_smooth_scroll: bool,
    pub smooth_scroll_offset_after_refresh_latency: FloatPoint,
    /// Whether the request the compositor awaits an answer to names the node, and the rect it asks for.
    pub in_flight_request_names_node: bool,
    pub in_flight_rect: IntRect,
}

/// What the compositor asks WebContent to record of a scroll node, if any of the node shows.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct FfiDesiredInterestRect {
    pub shows: bool,
    pub rect: IntRect,
}

/// Whether the compositor needs to ask WebContent to record the display list again, as a frame it presents shows it.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct FfiRecordingRefreshPlan {
    /// What the list records of a scroll node no longer holds what shows of it until a new recording could arrive.
    pub some_node_needs_refresh: bool,
    /// A scroll node that asked for the least it needs right after a jump waits to ask for its full rect.
    pub some_node_owes_request_after_jump: bool,
    /// What the request in flight asks for holds what shows of every node that needs a refresh.
    pub in_flight_request_holds: bool,
    /// How many device pixels of the content that shows lie outside what the list records, over every scroll node.
    pub checkerboarded_area: u64,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiVisualContextTreeInputs {
    pub device_pixels_per_css_pixel: f64,
    pub visual_viewport_offset_x: f64,
    pub visual_viewport_offset_y: f64,
    pub visual_viewport_scale: f64,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiVisualViewportTransform {
    pub matrix: FloatMatrix4x4,
    pub origin: FloatPoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiVisualAnimationTargetKind {
    Opacity,
    BackgroundColor,
    Filter,
    Transform,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiVisualAnimationPlaybackDirection {
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiVisualAnimationFillMode {
    None,
    Backwards,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiVisualAnimationTransformOperationKind {
    Translate,
    Translate3d,
    TranslateX,
    TranslateY,
    TranslateZ,
    Scale,
    Scale3d,
    ScaleX,
    ScaleY,
    ScaleZ,
    Rotate,
    RotateX,
    RotateY,
    RotateZ,
    Skew,
    SkewX,
    SkewY,
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct FfiCompositorAnimationPublishOutcome {
    /// The tree took the list; false when it already carried the same animations.
    pub published: bool,
    pub parameters_changed: bool,
    pub timing_anchors_changed: bool,
}

/// What a tree reports about the animations it carries, for test introspection.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct FfiVisualAnimationSummary {
    pub count: usize,
    pub local_time_at_anchor_ms_of_first: f64,
    pub share_timing_anchor: bool,
    pub targets_are_valid: bool,
}

/// Whether the content the tree's animations move can reach the viewport, and whether that answer
/// holds until the scene changes: it does once no finite animation is still running, since none can
/// then stop contributing on its own.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct FfiAnimatedContentViewportEffect {
    pub may_affect_viewport: bool,
    pub stable_until_scene_changes: bool,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiTestStickyConstraints {
    pub scroller: u32,
    pub has_parent_sticky: bool,
    pub parent_sticky: u32,
    pub position_relative_to_scroller: FloatPoint,
    pub border_box_size: FloatSize,
    pub scrollport_size: FloatSize,
    pub containing_block_region: FloatRect,
    pub needs_parent_offset_adjustment: bool,
    pub inset_top: OptionalF32,
    pub inset_right: OptionalF32,
    pub inset_bottom: OptionalF32,
    pub inset_left: OptionalF32,
}

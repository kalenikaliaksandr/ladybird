/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

pub mod async_scroll_metadata;
pub mod cache;
pub mod hit_test_items;
pub(crate) mod manifest;
pub mod masks;
pub mod paint;
pub(crate) mod scratch;
pub mod traversal;
pub(crate) mod verify;

use crate::css::css_enums;
use crate::layout::node_data::NodeSlotId;
use crate::layout::node_data::{NodeFlag, NodeKind};
use crate::painting::border_radii::BorderRadii;
use crate::painting::display_list::builder::{CommandRange, PendingInlineClip, RecordedDisplayList};
use crate::painting::display_list::commands::{ContextRef, DisplayListResourceId, SpatialNodeIndex};
use crate::painting::display_list::device_pixels::DevicePixelConverter;
use crate::painting::display_list::recorder::{DisplayListRecorder, VectorImageSource};
use crate::painting::hit_test::HitTestList;
use crate::painting::host::{
    FfiLayerImageFacts, FfiLayerImageList, FfiMaskDisplayListRegistration, FfiPaintHostCallbacks,
    FfiPaintRecordingStats, FfiRecordingInputs, FfiReplacedPaintFacts, FfiRootBackgroundSource,
    FfiVisualContextHostCallbacks, FfiVisualContextTreeInputs,
};
use crate::painting::paintable_data::{InlineBoxPieceRecord, PaintableData};
use crate::painting::paintable_rows::PaintableRowsRef;
use crate::painting::record::cache::{OpenCapture, RecordGen};
use crate::painting::visual_context::nested::NestedAssignments;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy)]
pub(crate) struct RecordingInputs {
    pub(crate) host: FfiRecordingInputs,
    pub(crate) device_pixels_per_css_pixel: f64,
    pub(crate) viewport_wheel_overflow_x: u8,
    pub(crate) viewport_wheel_overflow_y: u8,
    pub(crate) root_background_source: FfiRootBackgroundSource,
}

impl RecordingInputs {
    pub(crate) fn from_host_and_last_visual_context_update(
        host: FfiRecordingInputs,
        tree_inputs: FfiVisualContextTreeInputs,
        root_background_source: FfiRootBackgroundSource,
    ) -> Self {
        Self {
            host,
            device_pixels_per_css_pixel: tree_inputs.device_pixels_per_css_pixel,
            viewport_wheel_overflow_x: tree_inputs.viewport_wheel_overflow_x,
            viewport_wheel_overflow_y: tree_inputs.viewport_wheel_overflow_y,
            root_background_source,
        }
    }
}

impl std::ops::Deref for RecordingInputs {
    type Target = FfiRecordingInputs;

    fn deref(&self) -> &FfiRecordingInputs {
        &self.host
    }
}

impl std::ops::DerefMut for RecordingInputs {
    fn deref_mut(&mut self) -> &mut FfiRecordingInputs {
        &mut self.host
    }
}

#[derive(Default)]
pub struct RecordingOutput {
    pub recorded_structural_epoch: u64,
    // A default-constructed output's 0.0 never matches a real recording scale.
    pub recorded_device_pixels_per_css_pixel: f64,
    pub hit_test_list: HitTestList,
    pub display_list: Rc<RecordedDisplayList>,
    pub has_blocking_wheel_event_listeners: bool,
    pub wheel_event_listener_state_generation: u64,
    pub mask_display_lists: Vec<FfiMaskDisplayListRegistration>,
    pub resource_manifest: manifest::ResourceManifest,
    /// Placeholder vector image paints in `display_list`, patched at publish time.
    pub vector_image_paints: Vec<crate::painting::display_list::recorder::VectorImagePaintRequest>,
    pub recording_stats: FfiPaintRecordingStats,
    pub is_identical_to_cache_source: bool,
    pub(crate) capture_log_for_verification: Option<verify::CaptureLog>,
}

/// How the recorder paints an image, from the facts the host synced onto its row before the
/// recording.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ImagePaint {
    /// A raster frame the host registered during the sync.
    DecodedFrame {
        frame_id: u64,
        natural_width: i32,
        natural_height: i32,
    },
    /// A vector image, painted with a placeholder nested list the publish step has the host record
    /// at the painted size.
    VectorImage {
        source: VectorImageSource,
        accumulated_scale: libgfx_rust::FloatSize,
    },
}

impl ImagePaint {
    fn from_synced_facts(
        is_vector_image: bool,
        has_raster_frame: bool,
        frame_id: u64,
        (natural_width, natural_height): (i32, i32),
        vector_source: VectorImageSource,
        accumulated_scale: libgfx_rust::FloatSize,
    ) -> Option<Self> {
        if is_vector_image {
            Some(Self::VectorImage {
                source: vector_source,
                accumulated_scale,
            })
        } else if has_raster_frame {
            Some(Self::DecodedFrame {
                frame_id,
                natural_width,
                natural_height,
            })
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PaintPhase {
    Background,
    Border,
    TableCollapsedBorder,
    Foreground,
    Outline,
    Overlay,
}

impl PaintPhase {
    pub const COUNT: usize = 6;
}
pub(crate) struct NestedRecordingState {
    pub(crate) assignments: NestedAssignments,
}

pub(crate) struct DeferredWholeTapeSplice {
    pub(crate) source_display_list: Rc<RecordedDisplayList>,
    pub(crate) prologue_byte_count: usize,
    pub(crate) source_range: CommandRange,
}

pub struct PaintRecorder<'a> {
    pub(crate) layout_arena: &'a PaintableRowsRef<'a>,
    pub(crate) paint_state: &'a crate::painting::paint_state::PaintState,
    pub(crate) paint_host: &'a FfiPaintHostCallbacks,
    pub(crate) inputs: RecordingInputs,
    pub(crate) recorder: DisplayListRecorder,
    pub(crate) converter: DevicePixelConverter,
    pub(crate) draw_svg_geometry_for_clip_path: bool,
    pub(crate) visual_context_host: &'a FfiVisualContextHostCallbacks,
    pub(crate) nested: Option<NestedRecordingState>,
    pub(crate) nested_tree: Option<crate::painting::visual_context::VisualContextTree>,
    pub(crate) recording_into_context_free_nested_list: bool,
    pub(crate) prerecorded: crate::painting::record::masks::PrerecordedNestedDisplayLists,
    pub(crate) viewport: NodeSlotId,
    command_cache_source: Option<Rc<RecordingOutput>>,
    item_cache_source: Option<Rc<crate::painting::record::cache::HitTestItemCacheSource>>,
    hit_test_list_generation: u64,
    open_capture_stack: Vec<OpenCapture>,
    deferred_whole_tape_splice: Option<DeferredWholeTapeSplice>,
    pub(crate) blocking_wheel_event_region_count: u32,
    pub(crate) recording_stats: FfiPaintRecordingStats,
    uncacheable_paint_generation: u64,
    pub(crate) capture_log_for_verification: Option<verify::CaptureLog>,
    list: HitTestList,
    pub(crate) memo_tables: &'a RefCell<scratch::PerRecordingMemoTables>,
    pub(crate) completed_record_gen: RecordGen,
    pub(crate) all_paint_caches_dirty: bool,
    pub(crate) all_descendant_subtree_caches_dirty: bool,
    pub(crate) resource_manifest: &'a RefCell<manifest::ResourceManifest>,
    pub(crate) wheel_hit_test_target_cache: HashMap<NodeSlotId, SpatialNodeIndex>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct BasePaintFacts {
    pub is_visible: bool,
    pub empty_cells_property_applies: bool,
    pub has_backdrop_filter: bool,
    pub paints_border_image: bool,
}

impl<'a> PaintRecorder<'a> {
    pub(crate) fn mark_open_captures_unsplicable(&mut self) {
        self.uncacheable_paint_generation = self
            .uncacheable_paint_generation
            .checked_add(1)
            .expect("uncacheable paint generation overflowed");
    }

    pub(crate) fn data(&self, paintable: NodeSlotId) -> &PaintableData {
        self.layout_arena.paintable_data(paintable)
    }

    pub(crate) fn layout_node_shell(&self, paintable: NodeSlotId) -> *mut std::ffi::c_void {
        self.layout_arena.shell_if_live(paintable)
    }

    pub(crate) fn hit_test_facts(&mut self, paintable: NodeSlotId) -> hit_test_items::HitTestFacts {
        self.paintable_facts(paintable)
    }

    fn paintable_facts(&mut self, paintable: NodeSlotId) -> hit_test_items::HitTestFacts {
        if let Some(facts) = self.memo_tables.borrow().hit_test_facts(paintable) {
            return facts;
        }
        let facts = hit_test_items::hit_test_facts(self.layout_arena, paintable, &self.inputs);
        self.memo_tables.borrow_mut().set_hit_test_facts(paintable, facts);
        facts
    }

    /// The id the display list names `font` by. The font itself reaches the host through the
    /// resource manifest once the recording has returned.
    pub(crate) fn register_font(&self, font: &libgfx_rust::font::RetainedFont) -> u64 {
        self.resource_manifest.borrow_mut().register_font(font)
    }

    /// Finishes a nested recording and lists it in the resource manifest under a freshly minted id.
    pub(crate) fn publish_nested_display_list(
        &self,
        recorder: DisplayListRecorder,
        tree: crate::painting::visual_context::VisualContextTree,
    ) -> DisplayListResourceId {
        let finished = recorder.finish();
        let mask_registrations = finished
            .mask_display_lists
            .into_iter()
            .map(FfiMaskDisplayListRegistration::from)
            .collect();
        self.resource_manifest.borrow_mut().publish_nested_display_list(
            finished.recorded,
            tree,
            mask_registrations,
            finished.vector_image_paints,
        )
    }

    /// The host's facts about a replaced box's content, synced before the recording.
    pub(crate) fn replaced_paint_facts(&self, paintable: NodeSlotId) -> FfiReplacedPaintFacts {
        if !self.layout_arena.paintable_row_is_populated(paintable) {
            return FfiReplacedPaintFacts::default();
        }
        self.layout_arena
            .paintable_side_data(paintable)
            .replaced_paint_facts
            .as_deref()
            .copied()
            .unwrap_or_default()
    }

    /// How to paint an image box's or SVG image's decoded image, from the facts the host synced.
    pub(crate) fn replaced_image_paint(
        &self,
        paintable: NodeSlotId,
        accumulated_scale: libgfx_rust::FloatSize,
    ) -> Option<ImagePaint> {
        let facts = self.replaced_paint_facts(paintable);
        ImagePaint::from_synced_facts(
            facts.is_vector_image,
            facts.has_raster_frame,
            facts.frame_id,
            (facts.natural_frame_width, facts.natural_frame_height),
            VectorImageSource::ReplacedImage { paintable },
            accumulated_scale,
        )
    }

    /// The host's facts about one of `paintable`'s layer images, synced before the recording. The
    /// document background paints the body's background layers.
    pub(crate) fn layer_image_facts(
        &self,
        paintable: NodeSlotId,
        list: FfiLayerImageList,
        computed_index: u32,
    ) -> Option<FfiLayerImageFacts> {
        let (row, list) = if list == FfiLayerImageList::DocumentBackground {
            (
                self.inputs.root_background_source.body_layout_node,
                FfiLayerImageList::Background,
            )
        } else {
            (paintable, list)
        };
        if row.is_invalid() || !self.layout_arena.paintable_row_is_populated(row) {
            return None;
        }
        self.layout_arena
            .paintable_side_data(row)
            .layer_images
            .iter()
            .find(|facts| facts.list == list && facts.computed_index == computed_index)
            .copied()
    }

    /// Whether the layer image is a vector image, whose nested list the publish step has the host
    /// record at the painted size.
    pub(crate) fn layer_image_is_vector(
        &self,
        paintable: NodeSlotId,
        list: FfiLayerImageList,
        computed_index: u32,
    ) -> bool {
        self.layer_image_facts(paintable, list, computed_index)
            .is_some_and(|facts| facts.is_vector_image)
    }

    /// The layer image's facts when it is a `url()` image with a raster frame the host registered
    /// during the sync.
    pub(crate) fn layer_image_current_frame(
        &self,
        paintable: NodeSlotId,
        list: FfiLayerImageList,
        computed_index: u32,
    ) -> Option<FfiLayerImageFacts> {
        self.layer_image_facts(paintable, list, computed_index)
            .filter(|facts| facts.is_image_style_value && facts.has_raster_frame)
    }

    /// How to paint a layer image, from the facts the host synced.
    pub(crate) fn layer_image_paint(
        &self,
        paintable: NodeSlotId,
        list: FfiLayerImageList,
        computed_index: u32,
        accumulated_scale: libgfx_rust::FloatSize,
    ) -> Option<ImagePaint> {
        let facts = self.layer_image_facts(paintable, list, computed_index)?;
        ImagePaint::from_synced_facts(
            facts.is_vector_image,
            facts.has_raster_frame,
            facts.frame_id,
            (facts.natural_frame_width, facts.natural_frame_height),
            VectorImageSource::LayerImage {
                paintable,
                list,
                computed_index,
            },
            accumulated_scale,
        )
    }

    /// The scroll offset of `paintable` as a scroll container, zero for a box that owns no scroll
    /// node. Read from the scroll state the host refreshes before every recording.
    pub(crate) fn scroll_offset(&self, paintable: NodeSlotId) -> crate::css::css_pixels::CssPixelPoint {
        use crate::painting::display_list::commands::VISUAL_VIEWPORT_NODE_INDEX;
        let own_scroll_node = self.data(paintable).own_scroll_node_index;
        if own_scroll_node == VISUAL_VIEWPORT_NODE_INDEX {
            return crate::css::css_pixels::CssPixelPoint::default();
        }
        let visual_context = &self.paint_state.visual_context;
        let Some(tree) = visual_context.tree.as_ref() else {
            return crate::css::css_pixels::CssPixelPoint::default();
        };
        let slot = tree.scroll_state_slot_for_node(own_scroll_node);
        let own_offset = visual_context.scroll_state.state_at_slot(slot).own_offset;
        crate::css::css_pixels::CssPixelPoint::new(-own_offset.x, -own_offset.y)
    }

    /// The focused text control's selection as `(start, end)` when `node` is one of its text
    /// node's committed rows.
    pub(crate) fn text_control_selection(&self, node: NodeSlotId) -> Option<(usize, usize)> {
        let control = &self.inputs.focused_text_control;
        control.text_nodes[..control.text_node_count.min(control.text_nodes.len())]
            .contains(&node)
            .then_some((control.start, control.end))
    }

    /// The `::selection` style of a selected text node, as the host resolved it before the
    /// recording (`layout_arena_sync_selection_styles`).
    pub(crate) fn selection_style(&self, node: NodeSlotId) -> Rc<paint::text::SelectionStyleAnswer> {
        self.paint_state
            .selection_styles
            .get(&node)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn nested_recording_session(
        &self,
        recorder: DisplayListRecorder,
        nested: Option<NestedRecordingState>,
        nested_tree: Option<crate::painting::visual_context::VisualContextTree>,
        draw_svg_geometry_for_clip_path: bool,
    ) -> PaintRecorder<'a> {
        PaintRecorder {
            layout_arena: self.layout_arena,
            paint_state: self.paint_state,
            paint_host: self.paint_host,
            inputs: self.inputs,
            recorder,
            converter: self.converter,
            draw_svg_geometry_for_clip_path,
            visual_context_host: self.visual_context_host,
            nested,
            nested_tree,
            recording_into_context_free_nested_list: false,
            prerecorded: crate::painting::record::masks::PrerecordedNestedDisplayLists::default(),
            viewport: self.viewport,
            command_cache_source: None,
            item_cache_source: None,
            hit_test_list_generation: self.hit_test_list_generation,
            open_capture_stack: Vec::new(),
            deferred_whole_tape_splice: None,
            blocking_wheel_event_region_count: 0,
            recording_stats: FfiPaintRecordingStats::default(),
            uncacheable_paint_generation: 0,
            capture_log_for_verification: None,
            list: HitTestList::default(),
            memo_tables: self.memo_tables,
            completed_record_gen: self.completed_record_gen,
            all_paint_caches_dirty: self.all_paint_caches_dirty,
            all_descendant_subtree_caches_dirty: self.all_descendant_subtree_caches_dirty,
            resource_manifest: self.resource_manifest,
            wheel_hit_test_target_cache: HashMap::new(),
        }
    }

    pub(crate) fn border_radii(&mut self, paintable: NodeSlotId) -> BorderRadii {
        let Some(style) = self.layout_arena.node_style_if_live(paintable) else {
            return BorderRadii::default();
        };
        crate::painting::visual_context::node_values::border_radii_data(style, self.layout_arena, paintable)
    }

    pub(crate) fn piece_border_radii(&mut self, paintable: NodeSlotId, piece: &InlineBoxPieceRecord) -> BorderRadii {
        let Some(style) = self.layout_arena.node_style_if_live(paintable) else {
            return BorderRadii::default();
        };
        crate::painting::visual_context::node_values::piece_border_radii_data(
            style,
            piece.border_box_rect.width,
            piece.border_box_rect.height,
            piece.present_edges,
        )
    }

    pub(crate) fn base_paint_facts(&mut self, paintable: NodeSlotId) -> BasePaintFacts {
        if let Some(facts) = self.memo_tables.borrow().base_paint_facts(paintable) {
            return facts;
        }
        let Some(style) = self.layout_arena.node_style_if_live(paintable) else {
            let facts = BasePaintFacts::default();
            self.memo_tables.borrow_mut().set_base_paint_facts(paintable, facts);
            return facts;
        };
        let effects = style.effects();
        let retains_animated_content =
            self.layout_arena.node_flags_if_live(paintable) & NodeFlag::HasAnimatedOpacityOrTransform as u32 != 0;
        let is_visible = style.visibility() == crate::css::css_enums::visibility::VISIBLE
            && (effects.opacity != 0.0 || retains_animated_content);
        let empty_cells_property_applies = self.display(paintable).is_internal_table()
            && style.empty_cells() == crate::css::css_enums::empty_cells::HIDE
            && crate::painting::paint_order::first_paint_child(self.layout_arena, paintable).is_none();
        let has_backdrop_filter = effects.backdrop_filter.operations.length != 0;
        let paints_border_image = crate::painting::style_queries::handle_value(&style.border().border_image_source)
            .is_some_and(|source| matches!(source, crate::css::style_value::StyleValueData::Image { .. }));
        let facts = BasePaintFacts {
            is_visible,
            empty_cells_property_applies,
            has_backdrop_filter,
            paints_border_image,
        };
        self.memo_tables.borrow_mut().set_base_paint_facts(paintable, facts);
        facts
    }

    fn has_stacking_context(&self, paintable: NodeSlotId) -> bool {
        self.data(paintable).establishes_stacking_context
    }

    fn layout_kind(&self, paintable: NodeSlotId) -> Option<NodeKind> {
        self.layout_arena.node_kind_if_live(paintable)
    }

    fn display(&self, paintable: NodeSlotId) -> crate::css::display::FfiDisplay {
        crate::painting::style_queries::display(self.layout_arena, paintable)
    }

    fn visibility_is_visible(&self, paintable: NodeSlotId) -> bool {
        self.layout_arena
            .node_style_if_live(paintable)
            .is_none_or(|style| style.visibility() == css_enums::visibility::VISIBLE)
    }

    pub(crate) fn is_visible(&mut self, paintable: NodeSlotId) -> bool {
        self.base_paint_facts(paintable).is_visible
    }

    pub(crate) fn visible_for_hit_testing(&mut self, paintable: NodeSlotId) -> bool {
        self.paintable_facts(paintable).visible_for_hit_testing
    }

    fn is_replaced_box(&self, paintable: NodeSlotId) -> bool {
        crate::painting::style_queries::is_replaced_box(self.layout_arena, paintable)
    }

    pub(crate) fn with_context<R>(&mut self, context: ContextRef, paint: impl FnOnce(&mut Self) -> R) -> R {
        let previous_context = self.recorder.accumulated_visual_context();
        self.recorder.set_accumulated_visual_context(context);
        let result = paint(self);
        self.recorder.set_accumulated_visual_context(previous_context);
        result
    }

    pub(crate) fn record_with_inline_clips<R>(
        &mut self,
        inline_clips: &[PendingInlineClip],
        paint: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let enclosing_scope_clip_count = self.recorder.ambient_inline_clip_depth();
        self.recorder.push_ambient_inline_clips(inline_clips);
        let result = paint(self);
        self.recorder.truncate_ambient_inline_clips(enclosing_scope_clip_count);
        result
    }

    pub(crate) fn own_context(&self, paintable: NodeSlotId) -> ContextRef {
        if let Some(nested) = &self.nested
            && let Some((own, _)) = nested.assignments.paintable_contexts.get(&paintable.index)
        {
            return *own;
        }
        self.data(paintable).accumulated_visual_context
    }

    pub(crate) fn accumulated_2d_scale_at(&self, spatial: SpatialNodeIndex) -> libgfx_rust::FloatSize {
        let tree = self
            .nested_tree
            .as_ref()
            .or(self.paint_state.visual_context.tree.as_deref())
            .expect("recording runs against a visual context tree");
        tree.accumulated_2d_scale(
            spatial,
            &[],
            crate::painting::visual_context::IncludeVisualViewportTransform::No,
        )
    }

    pub(crate) fn own_accumulated_2d_scale(&self, paintable: NodeSlotId) -> libgfx_rust::FloatSize {
        self.accumulated_2d_scale_at(self.own_context(paintable).spatial)
    }
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::*;
use libcompositing_rust::display_list::builder::DisplayListBuilder;
use libcompositing_rust::display_list::commands::{
    ClipNodeIndex, ContextRef, DisplayListCommand, DrawCanvas, DrawVideoFrame, EffectNodeIndex, FillRect,
    VISUAL_VIEWPORT_NODE_INDEX,
};
use libcompositing_rust::scrolling::{AccumulatedVisualContextTree, ScrollStateSnapshot};
use libcompositing_rust::visual_context::{
    EffectNodeData, EffectsData, TransformData, TransformDataRole, VisualContextTree,
};
use libgfx_rust::{Color, CompositingAndBlendingOperator, FloatMatrix4x4, FloatPoint, FloatRect, ScalingMode};

const VIEWPORT_SIZE: IntSize = IntSize {
    width: 800,
    height: 600,
};
const VIEWPORT: IntRect = IntRect::new(0, 0, 800, 600);
const CANVAS_ID: CanvasId = CanvasId(1);
const VIDEO_SINK_ID: VideoSinkResourceId = VideoSinkResourceId(7);

fn tree_with_root_isolation_effect() -> (Arc<VisualContextTree>, ContextRef) {
    let mut tree = VisualContextTree::create(TransformData {
        matrix: FloatMatrix4x4::identity(),
        origin: FloatPoint::default(),
        sorting_context_root_index: None,
        flattens_inherited_transform: false,
        role: TransformDataRole::CssTransform,
        synthetic_plane: false,
        establishes_sorting_context: false,
        bounds: FloatRect::default(),
    });
    let root_isolation_effect = tree.append_effect(
        EffectNodeData::Effects(EffectsData {
            isolation: false,
            opacity: 1.0,
            blend_mode: CompositingAndBlendingOperator::Normal,
            filter: None,
            backdrop_filter: None,
        }),
        EffectNodeIndex::NONE,
        VISUAL_VIEWPORT_NODE_INDEX,
        ClipNodeIndex::NONE,
    );
    tree.root_isolation_effect = Some(root_isolation_effect);
    let root = ContextRef {
        effect: root_isolation_effect,
        ..ContextRef::default()
    };
    (Arc::new(tree), root)
}

fn fill(rect: IntRect) -> FillRect {
    FillRect {
        rect,
        color: Color::from_rgb(0, 128, 0),
        compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
        background_color_animation_effect: EffectNodeIndex::NONE,
    }
}

fn canvas(rect: IntRect) -> DrawCanvas {
    DrawCanvas {
        dst_rect: rect,
        canvas_id: CANVAS_ID,
        scaling_mode: ScalingMode::NearestNeighbor,
    }
}

struct TestList {
    builder: DisplayListBuilder,
    context: ContextRef,
}

impl TestList {
    fn new(context: ContextRef) -> Self {
        Self {
            builder: DisplayListBuilder::new(),
            context,
        }
    }

    fn with<C: DisplayListCommand>(mut self, command: C) -> Self {
        self.builder.append(&command, &[], self.context);
        self
    }

    fn storage(self) -> Arc<DisplayListStorage> {
        DisplayListStorage::from_recorded(Arc::new(self.builder.finish()))
    }
}

fn canvas_under_box(root: ContextRef) -> Arc<DisplayListStorage> {
    TestList::new(root)
        .with(fill(VIEWPORT))
        .with(canvas(IntRect::new(100, 100, 200, 100)))
        .with(fill(IntRect::new(150, 150, 20, 20)))
        .storage()
}

fn at_milliseconds(milliseconds: i64) -> MonotonicTime {
    MonotonicTime::from_nanoseconds(Duration::from_milliseconds(milliseconds).nanoseconds())
}

fn request_at(milliseconds: i64) -> FrameRequest {
    FrameRequest {
        viewport_size: VIEWPORT_SIZE,
        forced_damage_rect: IntRect::default(),
        now: at_milliseconds(milliseconds),
        presents_to_client: true,
        window_server_may_show_media: false,
        window_server_may_show_high_dynamic_range: true,
    }
}

fn shown_by_window_server(request: FrameRequest) -> FrameRequest {
    FrameRequest {
        window_server_may_show_media: true,
        ..request
    }
}

fn canvas_at_generation(content_generation: u64) -> LiveMediaSample {
    LiveMediaSample {
        canvases: vec![CanvasSample {
            id: CANVAS_ID,
            content_generation,
            has_window_server_surface: true,
        }],
        ..LiveMediaSample::default()
    }
}

struct Presenter {
    tree: Arc<VisualContextTree>,
    planner: FramePlanner,
    last_frame: Option<(Arc<DisplayListStorage>, FrameInputs)>,
}

impl Presenter {
    fn new(tree: Arc<VisualContextTree>) -> Self {
        Self {
            tree,
            planner: FramePlanner::default(),
            last_frame: None,
        }
    }

    fn frame_inputs(&self) -> FrameInputs {
        FrameInputs {
            visual_context_tree: AccumulatedVisualContextTree::from_arc(self.tree.clone()),
            presented_scroll_state_snapshot: ScrollStateSnapshot::from_device_offsets(Vec::new()),
            scrollbars: Vec::new(),
            viewport_scroll_node_index: None,
        }
    }

    fn plan(
        &mut self,
        display_list: &Arc<DisplayListStorage>,
        request: FrameRequest,
        sample: LiveMediaSample,
    ) -> FramePlan {
        let frame_inputs = self.frame_inputs();
        let last_frame = self
            .last_frame
            .as_ref()
            .map(|(last_display_list, last_frame_inputs)| LastFrame {
                frame_inputs: last_frame_inputs,
                display_list: last_display_list,
                viewport_size: VIEWPORT_SIZE,
                is_same_display_list: Arc::ptr_eq(last_display_list, display_list),
                has_other_surface_clear_color: false,
            });
        self.planner
            .plan(&request, &frame_inputs, display_list, last_frame.as_ref(), sample)
            .clone()
    }

    fn commit(&mut self, display_list: &Arc<DisplayListStorage>) -> Option<Duration> {
        let release_after = self.planner.commit();
        self.last_frame = Some((display_list.clone(), self.frame_inputs()));
        release_after
    }

    fn frame(
        &mut self,
        display_list: &Arc<DisplayListStorage>,
        request: FrameRequest,
        sample: LiveMediaSample,
    ) -> FramePlan {
        let plan = self.plan(display_list, request, sample);
        self.commit(display_list);
        plan
    }

    fn with_canvas_layer(
        display_list: &Arc<DisplayListStorage>,
        tree: Arc<VisualContextTree>,
        request: impl Fn(i64) -> FrameRequest,
    ) -> (Self, FramePlan) {
        let mut presenter = Self::new(tree);
        assert!(
            !presenter
                .frame(display_list, request(0), canvas_at_generation(1))
                .paints_from_raster_slots
        );
        assert!(
            !presenter
                .frame(display_list, request(100), canvas_at_generation(2))
                .paints_from_raster_slots
        );
        let plan = presenter.frame(display_list, request(200), canvas_at_generation(3));
        (presenter, plan)
    }
}

fn media_rect(presenter: &Presenter) -> IntRect {
    presenter.planner.committed_layer_plan().unwrap().layers[1].rect
}

fn painted_rect(presenter: &Presenter) -> IntRect {
    presenter.planner.committed_layer_plan().unwrap().layers[2].rect
}

#[test]
fn a_canvas_gets_raster_slots_once_it_keeps_updating() {
    let (tree, root) = tree_with_root_isolation_effect();
    let display_list = canvas_under_box(root);
    let (presenter, plan) = Presenter::with_canvas_layer(&display_list, tree, request_at);
    assert!(plan.paints_from_raster_slots);
    assert_eq!(plan.raster_slot_count, 3);
    assert_eq!(plan.damage_rect, VIEWPORT);
    assert_eq!(
        plan.steps,
        [
            FrameStep::RasterizeSlot {
                slot: 0,
                rect: VIEWPORT,
                covers_viewport: true,
                repaint: SlotRepaint::Whole,
            },
            FrameStep::ReleaseSlot { slot: 1 },
            FrameStep::RasterizeSlot {
                slot: 2,
                rect: painted_rect(&presenter),
                covers_viewport: false,
                repaint: SlotRepaint::Whole,
            },
            FrameStep::DrawSlotIntoStore {
                slot: 0,
                blend: SlotBlend::Source,
            },
            FrameStep::ReplaySlotRecordsIntoStore {
                slot: 1,
                rect: media_rect(&presenter),
            },
            FrameStep::DrawSlotIntoStore {
                slot: 2,
                blend: SlotBlend::SourceOver,
            },
            FrameStep::PaintCompositorOverlaysIntoStore,
        ]
    );
}

#[test]
fn a_canvas_update_under_a_kept_plan_repaints_only_the_canvas() {
    let (tree, root) = tree_with_root_isolation_effect();
    let display_list = canvas_under_box(root);
    let (mut presenter, _) = Presenter::with_canvas_layer(&display_list, tree, request_at);
    let plan = presenter.frame(&display_list, request_at(300), canvas_at_generation(4));
    assert_eq!(plan.damage_rect, media_rect(&presenter));
    for step in &plan.steps {
        if let FrameStep::RasterizeSlot { repaint, .. } = step {
            assert!(
                matches!(repaint, SlotRepaint::Within(rect) if rect.is_empty()),
                "{step:?}"
            );
        }
    }
}

#[test]
fn a_changed_command_repaints_the_slot_that_draws_it_within_its_damage() {
    let (tree, root) = tree_with_root_isolation_effect();
    let display_list = canvas_under_box(root);
    let (mut presenter, _) = Presenter::with_canvas_layer(&display_list, tree, request_at);
    let moved_box = TestList::new(root)
        .with(fill(VIEWPORT))
        .with(canvas(IntRect::new(100, 100, 200, 100)))
        .with(fill(IntRect::new(152, 150, 20, 20)))
        .storage();
    let plan = presenter.frame(&moved_box, request_at(300), canvas_at_generation(3));
    assert!(plan.paints_from_raster_slots);
    let repaint_of_slot = |slot: usize| {
        plan.steps.iter().find_map(|step| match *step {
            FrameStep::RasterizeSlot {
                slot: rasterized_slot,
                repaint,
                ..
            } if rasterized_slot == slot => Some(repaint),
            _ => None,
        })
    };
    assert!(matches!(repaint_of_slot(0), Some(SlotRepaint::Within(rect)) if rect.is_empty()));
    assert!(matches!(repaint_of_slot(2), Some(SlotRepaint::Within(rect)) if !rect.is_empty()));
}

#[test]
fn the_window_server_shows_the_canvas_and_the_slots_above_it() {
    let (tree, root) = tree_with_root_isolation_effect();
    let display_list = canvas_under_box(root);
    let (presenter, plan) = Presenter::with_canvas_layer(&display_list, tree, |milliseconds| {
        shown_by_window_server(request_at(milliseconds))
    });
    assert_eq!(
        plan.steps[3..],
        [
            FrameStep::DrawSlotIntoStore {
                slot: 0,
                blend: SlotBlend::Source,
            },
            FrameStep::WithdrawSlotFromWindowServer { slot: 0 },
            FrameStep::WithdrawSlotFromWindowServer { slot: 1 },
            FrameStep::PresentSlotToWindowServer {
                slot: 2,
                rect: painted_rect(&presenter),
            },
            FrameStep::PresentCompositorOverlaysToWindowServer,
        ]
    );
    let sublayers: Vec<_> = presenter
        .planner
        .sublayers()
        .iter()
        .map(|sublayer| (sublayer.identity, sublayer.content, sublayer.placement.is_some()))
        .collect();
    assert_eq!(
        sublayers,
        [
            (
                CANVAS_SUBLAYER_IDENTITY_TAG | 1,
                SublayerContent::Canvas(CANVAS_ID),
                true
            ),
            (
                RASTER_SLOT_SUBLAYER_IDENTITY_TAG | 2,
                SublayerContent::RasterSlot(2),
                false
            ),
            (
                COMPOSITOR_OVERLAYS_SUBLAYER_IDENTITY,
                SublayerContent::CompositorOverlays,
                false
            ),
        ]
    );
}

#[test]
fn a_canvas_update_shown_by_the_window_server_repaints_nothing_of_the_store() {
    let (tree, root) = tree_with_root_isolation_effect();
    let display_list = canvas_under_box(root);
    let (mut presenter, _) = Presenter::with_canvas_layer(&display_list, tree, |milliseconds| {
        shown_by_window_server(request_at(milliseconds))
    });
    let plan = presenter.frame(
        &display_list,
        shown_by_window_server(request_at(300)),
        canvas_at_generation(4),
    );
    assert!(plan.damage_rect.is_empty());
    assert!(plan.only_window_server_content_changed);
}

#[test]
fn the_window_server_starting_or_stopping_to_show_media_repaints_every_slot() {
    let (tree, root) = tree_with_root_isolation_effect();
    let display_list = canvas_under_box(root);
    let (mut presenter, _) = Presenter::with_canvas_layer(&display_list, tree, |milliseconds| {
        shown_by_window_server(request_at(milliseconds))
    });
    let plan = presenter.frame(&display_list, request_at(300), canvas_at_generation(3));
    assert_eq!(plan.damage_rect, VIEWPORT);
    for step in &plan.steps {
        if let FrameStep::RasterizeSlot { repaint, .. } = step {
            assert_eq!(*repaint, SlotRepaint::Whole);
        }
    }
    assert!(presenter.planner.sublayers().is_empty());
}

#[test]
fn a_failed_slot_allocation_needs_the_whole_store_only_while_the_window_server_shows_media() {
    let (tree, root) = tree_with_root_isolation_effect();
    let display_list = canvas_under_box(root);
    let (mut presenter, _) = Presenter::with_canvas_layer(&display_list, tree.clone(), request_at);
    presenter.plan(&display_list, request_at(300), canvas_at_generation(4));
    assert!(!presenter.planner.did_fail_to_allocate_raster_slot());
    assert_eq!(presenter.commit(&display_list), None);

    let (mut presenter, _) = Presenter::with_canvas_layer(&display_list, tree, |milliseconds| {
        shown_by_window_server(request_at(milliseconds))
    });
    presenter.plan(
        &display_list,
        shown_by_window_server(request_at(300)),
        canvas_at_generation(4),
    );
    assert!(presenter.planner.did_fail_to_allocate_raster_slot());
    assert_eq!(presenter.commit(&display_list), None);
    assert!(presenter.planner.sublayers().is_empty());
}

#[test]
fn idle_slots_are_released_unless_the_window_server_shows_media() {
    let (tree, root) = tree_with_root_isolation_effect();
    let display_list = canvas_under_box(root);
    let (mut presenter, _) = Presenter::with_canvas_layer(&display_list, tree.clone(), |milliseconds| {
        shown_by_window_server(request_at(milliseconds))
    });
    assert!(!presenter.planner.release_idle_layers());

    let (mut presenter, _) = Presenter::with_canvas_layer(&display_list, tree, request_at);
    assert!(presenter.planner.release_idle_layers());
    let plan = presenter.frame(&display_list, request_at(4000), canvas_at_generation(3));
    assert!(!plan.paints_from_raster_slots);
    assert_eq!(plan.damage_rect, VIEWPORT);
    let plan = presenter.frame(&display_list, request_at(4100), canvas_at_generation(3));
    assert!(plan.damage_rect.is_empty());
}

#[test]
fn committing_layers_asks_for_their_release_after_media_stops_being_active() {
    let (tree, root) = tree_with_root_isolation_effect();
    let display_list = canvas_under_box(root);
    let (mut presenter, _) = Presenter::with_canvas_layer(&display_list, tree, request_at);
    presenter.plan(&display_list, request_at(300), canvas_at_generation(4));
    assert_eq!(
        presenter.commit(&display_list),
        Some(TIME_MEDIA_STAYS_ACTIVE_AFTER_AN_UPDATE)
    );
}

#[test]
fn each_drawing_of_a_canvas_is_a_sublayer_of_its_own() {
    let (tree, root) = tree_with_root_isolation_effect();
    let display_list = TestList::new(root)
        .with(fill(VIEWPORT))
        .with(canvas(IntRect::new(100, 100, 200, 100)))
        .with(canvas(IntRect::new(400, 300, 200, 100)))
        .storage();
    let (presenter, _) = Presenter::with_canvas_layer(&display_list, tree, |milliseconds| {
        shown_by_window_server(request_at(milliseconds))
    });
    let identities: Vec<u64> = presenter
        .planner
        .sublayers()
        .iter()
        .map(|sublayer| sublayer.identity)
        .collect();
    assert_eq!(
        identities,
        [
            CANVAS_SUBLAYER_IDENTITY_TAG | 1,
            CANVAS_SUBLAYER_IDENTITY_TAG | 1 | (1 << MEDIA_DRAWING_INDEX_SHIFT),
            COMPOSITOR_OVERLAYS_SUBLAYER_IDENTITY,
        ]
    );
}

#[test]
fn a_video_showing_high_dynamic_range_is_a_layer_from_its_first_frame() {
    let (tree, root) = tree_with_root_isolation_effect();
    let display_list = TestList::new(root)
        .with(fill(VIEWPORT))
        .with(DrawVideoFrame {
            dst_rect: IntRect::new(100, 100, 320, 180),
            video_sink_id: VIDEO_SINK_ID,
            scaling_mode: ScalingMode::Bilinear,
        })
        .storage();
    let sample = LiveMediaSample {
        videos: vec![VideoSample {
            id: VIDEO_SINK_ID,
            frame_id: Some(1),
            frame_has_window_server_surface: true,
            frame_holds_high_dynamic_range: true,
        }],
        ..LiveMediaSample::default()
    };

    let mut presenter = Presenter::new(tree.clone());
    let plan = presenter.frame(&display_list, shown_by_window_server(request_at(0)), sample.clone());
    assert!(plan.paints_from_raster_slots);
    assert_eq!(
        presenter.planner.sublayers()[0].content,
        SublayerContent::Video(VIDEO_SINK_ID)
    );

    let mut presenter = Presenter::new(tree);
    let without_high_dynamic_range = FrameRequest {
        window_server_may_show_high_dynamic_range: false,
        ..shown_by_window_server(request_at(0))
    };
    assert!(
        !presenter
            .frame(&display_list, without_high_dynamic_range, sample)
            .paints_from_raster_slots
    );
}

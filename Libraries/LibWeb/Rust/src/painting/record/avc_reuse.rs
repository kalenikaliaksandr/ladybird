/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Validate the preceding frame's owned output against AVC identity changes. The
//! journal is sparse; dependency masks and rejected operation indices are temporary
//! recording workspace. No reference vectors are retained per paint occurrence.

use super::RecordingOutput;
use super::cache::HitTestItemCacheSource;
use super::directory::OutputPoint;
use crate::painting::display_list::builder::{HEADER_SIZE, for_each_command, read_command};
use crate::painting::display_list::commands::*;
use crate::painting::visual_context::cache_changes::{CacheChanges, VisualContextNodeRef};
use crate::painting::visual_context::{VisualContextState, VisualContextTree};

#[derive(Default)]
pub(super) enum AvcReuseFilter {
    #[default]
    Unchanged,
    Full,
    Checked(Vec<u32>),
}

impl AvcReuseFilter {
    pub fn new(
        visual: &VisualContextState,
        source: Option<&RecordingOutput>,
        items: Option<&HitTestItemCacheSource>,
    ) -> Self {
        let Some(source) = source else { return Self::Full };
        let changes = match visual
            .cache_changes
            .since(source.recorded_structural_epoch, visual.structural_epoch())
        {
            CacheChanges::Unchanged => return Self::Unchanged,
            CacheChanges::Full => return Self::Full,
            CacheChanges::Nodes(nodes) => nodes,
        };
        let (Some(tree), Some(cache), Some(items)) = (visual.tree.as_deref(), source.paint_cache.as_ref(), items)
        else {
            return Self::Full;
        };
        let Some(invalid) = InvalidNodes::new(tree, changes.keys().copied()) else {
            return Self::Full;
        };
        let boundaries = &cache.directory.boundaries;
        if boundaries.last().is_none_or(|end| {
            end.commands as usize != source.display_list.bytes.len() || end.hits as usize != items.items.len()
        }) {
            return Self::Full;
        }
        let mut rejected = Vec::new();
        let mut nested = Vec::new();
        let mut operation = 0;
        for_each_command(&source.display_list.bytes, |header, offset, payload| {
            if !command_is_invalid(header, payload, &invalid, &mut nested) {
                return;
            }
            operation = operation_at_output(boundaries, operation, offset, |point| point.commands);
            // Producer boundaries enclose complete commands, including their inline groups.
            debug_assert!(offset + HEADER_SIZE + payload.len() <= boundaries[operation + 1].commands as usize);
            rejected.push(operation as u32);
        });
        operation = 0;
        for (index, item) in items.items.iter().enumerate() {
            if invalid.context(item.context) {
                operation = operation_at_output(boundaries, operation, index, |point| point.hits);
                rejected.push(operation as u32);
            }
        }
        rejected.sort_unstable();
        rejected.dedup();
        Self::Checked(rejected)
    }

    pub fn permits_any_reuse(&self) -> bool {
        !matches!(self, Self::Full)
    }

    pub fn permits_interval(&self, begin: u32, end: u32) -> bool {
        match self {
            Self::Unchanged => true,
            Self::Full => false,
            Self::Checked(rejected) => {
                let first = rejected.partition_point(|&operation| operation < begin);
                rejected.get(first).is_none_or(|&operation| operation >= end)
            }
        }
    }

    pub fn rejected_operations(&self) -> &[u32] {
        match self {
            Self::Checked(operations) => operations,
            Self::Unchanged | Self::Full => &[],
        }
    }
}

fn operation_at_output(
    boundaries: &[OutputPoint],
    mut operation: usize,
    output: usize,
    coordinate: impl Fn(OutputPoint) -> u32,
) -> usize {
    while coordinate(boundaries[operation + 1]) as usize <= output {
        operation += 1;
    }
    operation
}

struct InvalidNodes {
    spatial: Vec<bool>,
    clip: Vec<bool>,
    effect: Vec<bool>,
}

impl InvalidNodes {
    fn new(tree: &VisualContextTree, changes: impl Iterator<Item = VisualContextNodeRef>) -> Option<Self> {
        let mut invalid = Self {
            spatial: tree.spatial_nodes.iter().map(|node| !node.data.is_live()).collect(),
            clip: tree.clip_nodes.iter().map(|node| !node.data.is_live()).collect(),
            effect: tree.effect_nodes.iter().map(|node| !node.data.is_live()).collect(),
        };
        for changed in changes {
            let slot = match changed {
                VisualContextNodeRef::Spatial(index) => invalid.spatial.get_mut(index.0 as usize),
                VisualContextNodeRef::Clip(index) => invalid.clip.get_mut(index.0 as usize),
                VisualContextNodeRef::Effect(index) => invalid.effect.get_mut(index.0 as usize),
            };
            if let Some(slot) = slot {
                *slot = true;
            }
        }
        let spatial = tree.spatial_dependency_order_with_back_edges();
        let clip = tree.clip_dependency_order_with_back_edges();
        let effect = tree.effect_dependency_order_with_back_edges();
        if [&spatial, &clip, &effect]
            .iter()
            .any(|order| !order.back_edges.is_empty() || !order.dangling_references.is_empty())
        {
            return None;
        }
        // A context depending on a repurposed slot also needs validation, even if
        // its own numeric handle survived. Spatial dependencies include scroll,
        // sticky, anchor-shift and sorting-context references in addition to parents.
        for index in spatial.order {
            let mut changed = invalid.spatial(SpatialNodeIndex(index));
            crate::painting::visual_context::for_each_spatial_node_reference(
                SpatialNodeIndex(index),
                &tree.spatial_nodes[index as usize],
                |reference| changed |= invalid.spatial(reference),
            );
            invalid.spatial[index as usize] = changed;
        }
        for index in clip.order {
            let node = &tree.clip_nodes[index as usize];
            let changed =
                invalid.clip(ClipNodeIndex(index)) || invalid.clip(node.parent) || invalid.spatial(node.spatial);
            invalid.clip[index as usize] = changed;
        }
        for index in effect.order {
            let node = &tree.effect_nodes[index as usize];
            let changed = invalid.effect(EffectNodeIndex(index))
                || invalid.effect(node.parent)
                || invalid.spatial(node.spatial)
                || invalid.clip(node.local_clip);
            invalid.effect[index as usize] = changed;
        }
        Some(invalid)
    }

    fn spatial(&self, index: SpatialNodeIndex) -> bool {
        self.spatial.get(index.0 as usize).copied().unwrap_or(true)
    }
    fn clip(&self, index: ClipNodeIndex) -> bool {
        !index.is_none() && self.clip.get(index.0 as usize).copied().unwrap_or(true)
    }
    fn effect(&self, index: EffectNodeIndex) -> bool {
        !index.is_none() && self.effect.get(index.0 as usize).copied().unwrap_or(true)
    }
    fn context(&self, context: ContextRef) -> bool {
        self.spatial(context.spatial) || self.clip(context.clip) || self.effect(context.effect)
    }
}

fn command_is_invalid<'a>(
    header: &DisplayListCommandHeader,
    payload: &'a [u8],
    invalid: &InvalidNodes,
    nested: &mut Vec<&'a [u8]>,
) -> bool {
    nested.clear();
    if check_command_and_push_children(header, payload, invalid, nested) {
        return true;
    }
    while let Some(bytes) = nested.pop() {
        let mut changed = false;
        for_each_command(bytes, |header, _, payload| {
            if !changed {
                changed = check_command_and_push_children(header, payload, invalid, nested);
            }
        });
        if changed {
            nested.clear();
            return true;
        }
    }
    false
}

fn check_command_and_push_children<'a>(
    header: &DisplayListCommandHeader,
    payload: &'a [u8],
    invalid: &InvalidNodes,
    nested: &mut Vec<&'a [u8]>,
) -> bool {
    if invalid.context(header.context) {
        return true;
    }
    crate::painting::display_list::nested_records::for_each_nested_record_span(
        header.command_type,
        payload,
        |_, span| {
            if !span.is_empty() {
                nested.push(crate::painting::display_list::nested_records::span_bytes(payload, span));
            }
        },
    );
    // Deliberately exhaustive: adding a command requires auditing its payload's AVC
    // references. Resource IDs and external nested display lists use their own lifetime
    // contracts; only inline group commands share this visual-context namespace.
    match header.command_type {
        DisplayListCommandType::FillRect => {
            invalid.effect(read_command::<FillRect>(payload).background_color_animation_effect)
        }
        DisplayListCommandType::FillRectWithRoundedCorners => {
            invalid.effect(read_command::<FillRectWithRoundedCorners>(payload).background_color_animation_effect)
        }
        DisplayListCommandType::DeclareMaskContent => {
            invalid.effect(read_command::<DeclareMaskContent>(payload).effect)
        }
        DisplayListCommandType::CompositorScrollNode => {
            let command = read_command::<CompositorScrollNode>(payload);
            invalid.spatial(command.scroll_node_index) || invalid.spatial(command.parent_scroll_node_index)
        }
        DisplayListCommandType::CompositorSnapContainer => {
            invalid.spatial(read_command::<CompositorSnapContainer>(payload).scroll_node_index)
        }
        DisplayListCommandType::CompositorSnapArea => {
            invalid.spatial(read_command::<CompositorSnapArea>(payload).scroll_node_index)
        }
        DisplayListCommandType::CompositorWheelHitTestTarget => {
            invalid.spatial(read_command::<CompositorWheelHitTestTarget>(payload).target_scroll_node_index)
        }
        DisplayListCommandType::CompositorWheelHitTestTargetWithCornerRadii => invalid
            .spatial(read_command::<CompositorWheelHitTestTargetWithCornerRadii>(payload).target_scroll_node_index),
        DisplayListCommandType::CompositorViewportScrollbar => {
            invalid.spatial(read_command::<CompositorViewportScrollbar>(payload).scroll_node_index)
        }
        DisplayListCommandType::PaintScrollBar => {
            invalid.spatial(read_command::<PaintScrollBar>(payload).scroll_node_index)
        }
        DisplayListCommandType::DrawIsolatedGroup
        | DisplayListCommandType::DrawRepeatedTile
        | DisplayListCommandType::FillPath
        | DisplayListCommandType::StrokePath
        | DisplayListCommandType::DrawGlyphRun
        | DisplayListCommandType::PaintCaret
        | DisplayListCommandType::DrawScaledDecodedImageFrame
        | DisplayListCommandType::DrawRepeatedDecodedImageFrame
        | DisplayListCommandType::DrawTiledDecodedImageFrame
        | DisplayListCommandType::DrawCompositedContext
        | DisplayListCommandType::DrawCanvas
        | DisplayListCommandType::DrawVideoFrame
        | DisplayListCommandType::PaintLinearGradient
        | DisplayListCommandType::PaintRadialGradient
        | DisplayListCommandType::PaintConicGradient
        | DisplayListCommandType::PaintOuterBoxShadow
        | DisplayListCommandType::PaintInnerBoxShadow
        | DisplayListCommandType::PaintTextShadow
        | DisplayListCommandType::DrawEllipse
        | DisplayListCommandType::DrawLine
        | DisplayListCommandType::BackdropFilterRegion
        | DisplayListCommandType::DrawRect
        | DisplayListCommandType::PaintNestedDisplayList
        | DisplayListCommandType::CompositorMainThreadWheelEventRegion
        | DisplayListCommandType::CompositorBlockingWheelEventRegion => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::node_data::NodeSlotId;
    use crate::painting::display_list::builder::DisplayListBuilder;
    use crate::painting::hit_test::{HitTestItem, HitTestItemKind};
    use crate::painting::record::directory::{FramePaintCache, OwnerInputs, PaintDirectory};
    use crate::painting::record::program::{NO_INDEX, test_program};
    use crate::painting::visual_context::{
        ClipData, ClipNodeData, EffectNodeData, EffectsData, SpatialData, SpatialNode, TransformData, TransformDataRole,
    };
    use libgfx_rust::{Color, CompositingAndBlendingOperator, FloatMatrix4x4, FloatPoint, FloatRect, IntRect};
    use std::rc::Rc;
    use std::sync::Arc;

    fn transform() -> TransformData {
        TransformData {
            matrix: FloatMatrix4x4::identity(),
            origin: FloatPoint::default(),
            sorting_context_root_index: None,
            flattens_inherited_transform: false,
            role: TransformDataRole::CssTransform,
            synthetic_plane: false,
            establishes_sorting_context: false,
        }
    }

    fn tree() -> VisualContextTree {
        let mut tree = VisualContextTree::create(transform());
        for _ in 0..2 {
            tree.append_spatial(SpatialData::Transform(transform()), VISUAL_VIEWPORT_NODE_INDEX);
        }
        tree.structural_epoch = 1;
        tree
    }

    fn fill() -> FillRect {
        FillRect {
            rect: IntRect::default(),
            color: Color::default(),
            compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
            background_color_animation_effect: EffectNodeIndex::NONE,
        }
    }

    fn item(context: ContextRef) -> HitTestItem {
        HitTestItem {
            kind: HitTestItemKind::Box,
            paintable: NodeSlotId::new(1, 1),
            hit_node: NodeSlotId::new(1, 1),
            chrome_widget_kind: 0,
            text_fragment_index: None,
            caret_node: NodeSlotId::INVALID,
            caret_offset: 0,
            rect: Default::default(),
            caret_rect: Default::default(),
            caret_line_index: None,
            caret_line_rect: None,
            block_container_margin_rect: None,
            block_container: NodeSlotId::INVALID,
            context,
            border_radii: Default::default(),
            path: None,
            winding_rule: 0,
            writing_mode: 0,
            inline_axis_is_reverse: false,
            block_axis_is_reverse: false,
            containing_block: NodeSlotId::INVALID,
            can_produce_caret_position: false,
        }
    }

    fn source(hit_only: bool) -> (RecordingOutput, HitTestItemCacheSource) {
        let mut builder = DisplayListBuilder::default();
        builder.append(&fill(), &[], ContextRef::default());
        let first = builder.byte_size() as u32;
        let bad_context = ContextRef::spatial_only(SpatialNodeIndex(2));
        if !hit_only {
            builder.append(&fill(), &[], bad_context);
        }
        let last = builder.byte_size() as u32;
        let hit_count = u32::from(hit_only);
        let mut directory = PaintDirectory::new(6);
        for point in [
            OutputPoint::default(),
            OutputPoint {
                commands: first,
                hits: 0,
            },
            OutputPoint {
                commands: first,
                hits: 0,
            },
            OutputPoint {
                commands: last,
                hits: hit_count,
            },
            OutputPoint {
                commands: last,
                hits: hit_count,
            },
            OutputPoint {
                commands: last,
                hits: hit_count,
            },
        ] {
            directory.append(point, true, false, 0);
        }
        let output = RecordingOutput {
            recorded_structural_epoch: 1,
            display_list: Arc::new(builder.finish()),
            paint_cache: Some(FramePaintCache {
                program: test_program(&[(NodeSlotId::new(1, 1), NO_INDEX), (NodeSlotId::new(2, 1), 0)]),
                directory: Rc::new(directory),
                owner_inputs: Rc::new(vec![OwnerInputs::default(); 2]),
                record_gen: 1,
                topology_revision: 0,
                geometry_revision: 0,
            }),
            ..Default::default()
        };
        let items = HitTestItemCacheSource {
            items: Rc::new(if hit_only { vec![item(bad_context)] } else { vec![] }),
        };
        (output, items)
    }

    #[test]
    fn removal_rejects_only_the_referencing_producer_including_hit_only_output() {
        for hit_only in [false, true] {
            let mut tree = tree();
            tree.tombstone_spatial_slot(SpatialNodeIndex(2));
            tree.structural_epoch = 2;
            let mut visual = VisualContextState {
                tree: Some(Rc::new(tree)),
                ..Default::default()
            };
            visual.cache_changes.reset(1);
            visual.cache_changes.publish(1);
            visual
                .cache_changes
                .record(1, 2, &[VisualContextNodeRef::Spatial(SpatialNodeIndex(2))]);
            let (source, items) = source(hit_only);
            let filter = AvcReuseFilter::new(&visual, Some(&source), Some(&items));
            assert_eq!(filter.rejected_operations(), [3]);
            assert!(filter.permits_interval(0, 3));
            assert!(!filter.permits_interval(3, 4));
            assert!(!filter.permits_interval(0, 6));
            assert!(filter.permits_interval(4, 6));
        }
    }

    #[test]
    fn recycling_cannot_make_an_old_reference_valid_again() {
        let mut tree = tree();
        tree.tombstone_spatial_slot(SpatialNodeIndex(2));
        tree.release_quarantined_slots_after_recording();
        let (recycled, reused) = tree.allocate_spatial_slot();
        assert!(reused);
        assert_eq!(recycled, SpatialNodeIndex(2));
        tree.replace_spatial_node(
            recycled,
            SpatialNode {
                data: SpatialData::Transform(transform()),
                parent: VISUAL_VIEWPORT_NODE_INDEX,
            },
        );
        let invalid = InvalidNodes::new(&tree, [VisualContextNodeRef::Spatial(recycled)].into_iter()).unwrap();
        assert!(tree.spatial_is_live(recycled));
        assert!(invalid.spatial(recycled));
        assert!(!invalid.spatial(SpatialNodeIndex(1)));
    }

    #[test]
    fn repurposing_invalidates_dependent_spatial_clip_and_effect_contexts() {
        let mut tree = tree();
        let child = tree.append_spatial(SpatialData::Transform(transform()), SpatialNodeIndex(1));
        let clip = tree.append_clip(
            ClipNodeData::Rect(ClipData {
                rect: FloatRect::default(),
                corner_radii: Default::default(),
                mode: ClipMode::Intersect,
            }),
            ClipNodeIndex::NONE,
            child,
        );
        let effect = tree.append_effect(
            EffectNodeData::Effects(EffectsData {
                opacity: 1.0,
                blend_mode: CompositingAndBlendingOperator::Normal,
                filter: None,
                backdrop_filter: None,
            }),
            EffectNodeIndex::NONE,
            VISUAL_VIEWPORT_NODE_INDEX,
            clip,
        );
        let invalid =
            InvalidNodes::new(&tree, [VisualContextNodeRef::Spatial(SpatialNodeIndex(1))].into_iter()).unwrap();
        assert!(invalid.spatial(child));
        assert!(invalid.clip(clip));
        assert!(invalid.effect(effect));
        assert!(!invalid.spatial(SpatialNodeIndex(2)));
    }

    #[test]
    fn animation_mask_and_scroll_references_are_checked_outside_the_header() {
        let invalid = InvalidNodes {
            spatial: vec![false, true],
            clip: vec![],
            effect: vec![true],
        };
        fn check<C: DisplayListCommand>(command: C, invalid: &InvalidNodes) {
            let mut builder = DisplayListBuilder::default();
            builder.append(&command, &[], ContextRef::default());
            let list = builder.finish();
            let mut checked = false;
            for_each_command(&list.bytes, |header, _, payload| {
                assert!(command_is_invalid(header, payload, invalid, &mut Vec::new()));
                checked = true;
            });
            assert!(checked);
        }
        check(
            FillRect {
                background_color_animation_effect: EffectNodeIndex(0),
                ..fill()
            },
            &invalid,
        );
        check(
            DeclareMaskContent {
                effect: EffectNodeIndex(0),
                rect: IntRect::default(),
                content: DisplayListDataSpan::default(),
            },
            &invalid,
        );
        check(
            CompositorWheelHitTestTarget {
                document_id: UniqueNodeId(1),
                target_scroll_node_index: SpatialNodeIndex(1),
                rect: FloatRect::default(),
            },
            &invalid,
        );
        check(
            CompositorScrollNode {
                document_id: UniqueNodeId(1),
                scrollable_node_id: UniqueNodeId(2),
                scroll_node_index: VISUAL_VIEWPORT_NODE_INDEX,
                parent_scroll_node_index: SpatialNodeIndex(1),
                scrollport_rect: IntRect::default(),
                min_scroll_offset: FloatPoint::default(),
                max_scroll_offset: FloatPoint::default(),
                scroll_node_kind: CompositorScrollNodeKind::Element,
                pseudo_element_type: 0,
                is_viewport: false,
                can_be_wheel_scrolled_horizontally: false,
                can_be_wheel_scrolled_vertically: false,
            },
            &invalid,
        );
    }

    #[test]
    fn inline_groups_are_checked_even_when_the_outer_run_context_is_unchanged() {
        let mut inner = DisplayListBuilder::default();
        inner.append(&fill(), &[], ContextRef::spatial_only(SpatialNodeIndex(1)));
        let inner = inner.finish();
        let mut builder = DisplayListBuilder::default();
        builder.append(
            &DeclareMaskContent {
                rect: IntRect::default(),
                effect: EffectNodeIndex::NONE,
                content: DisplayListDataSpan {
                    offset: std::mem::size_of::<DeclareMaskContent>() as u32,
                    size: inner.bytes.len() as u32,
                },
            },
            &inner.bytes,
            ContextRef::default(),
        );
        let list = builder.finish();
        assert_eq!(list.command_runs.len(), 1);
        assert_eq!(list.command_runs[0].context, ContextRef::default());
        let invalid = InvalidNodes {
            spatial: vec![false, true],
            clip: vec![],
            effect: vec![],
        };
        for_each_command(&list.bytes, |header, _, payload| {
            assert!(command_is_invalid(header, payload, &invalid, &mut Vec::new()));
        });
    }

    #[test]
    fn isolated_group_masks_and_svg_pattern_tiles_keep_their_nested_references() {
        let mut inner = DisplayListBuilder::default();
        inner.append(&fill(), &[], ContextRef::spatial_only(SpatialNodeIndex(1)));
        let inner = inner.finish();
        fn check<C: DisplayListCommand>(command: C, data: &[u8]) {
            let mut builder = DisplayListBuilder::default();
            builder.append(&command, data, ContextRef::default());
            let list = builder.finish();
            let invalid = InvalidNodes {
                spatial: vec![false, true],
                clip: vec![],
                effect: vec![],
            };
            for_each_command(&list.bytes, |header, _, payload| {
                assert!(command_is_invalid(header, payload, &invalid, &mut Vec::new()));
            });
        }
        check(
            DrawIsolatedGroup {
                clip_rect: OptionalFloatRect::none(),
                content: DisplayListDataSpan::default(),
                mask: DisplayListDataSpan {
                    offset: std::mem::size_of::<DrawIsolatedGroup>() as u32,
                    size: inner.bytes.len() as u32,
                },
                filter: DisplayListDataSpan::default(),
                opacity: 1.0,
                compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
                mask_kind: libgfx_rust::MaskKind::Alpha,
            },
            &inner.bytes,
        );
        check(
            FillPath {
                path_bounding_rect: FloatRect::default(),
                path_data: DisplayListDataSpan::default(),
                opacity: 1.0,
                paint_kind: PathPaintKind::PaintStyle,
                color: Color::default(),
                paint_style: DisplayListPaintStyle {
                    paint_style_type: DisplayListPaintStyleType::Pattern,
                    pattern_tile: DisplayListDataSpan {
                        offset: std::mem::size_of::<FillPath>() as u32,
                        size: inner.bytes.len() as u32,
                    },
                    ..Default::default()
                },
                winding_rule: libgfx_rust::WindingRule::Nonzero,
                should_anti_alias: libgfx_rust::ShouldAntiAlias::Yes,
                compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
            },
            &inner.bytes,
        );
    }

    #[test]
    fn zero_output_boundaries_do_not_hide_the_producer_of_invalid_output() {
        let points = [
            OutputPoint::default(),
            OutputPoint::default(),
            OutputPoint { commands: 16, hits: 0 },
            OutputPoint { commands: 16, hits: 1 },
            OutputPoint { commands: 16, hits: 1 },
            OutputPoint { commands: 32, hits: 2 },
        ];
        assert_eq!(operation_at_output(&points, 0, 0, |p| p.commands), 1);
        assert_eq!(operation_at_output(&points, 1, 16, |p| p.commands), 4);
        assert_eq!(operation_at_output(&points, 0, 0, |p| p.hits), 2);
        assert_eq!(operation_at_output(&points, 2, 1, |p| p.hits), 4);
    }
}

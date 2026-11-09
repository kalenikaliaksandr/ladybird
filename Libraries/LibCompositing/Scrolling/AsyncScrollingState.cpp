/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/JsonArray.h>
#include <AK/JsonObject.h>
#include <LibCompositing/DisplayList/DisplayList.h>
#include <LibCompositing/Scrolling/AsyncScrollingState.h>
#include <LibCompositing/Scrolling/ScrollState.h>

namespace Compositing {

Web::AsyncScrollNodeKind async_scroll_node_kind_for(Compositing::CompositorScrollNodeKind kind)
{
    switch (kind) {
    case Compositing::CompositorScrollNodeKind::Viewport:
        return Web::AsyncScrollNodeKind::Viewport;
    case Compositing::CompositorScrollNodeKind::Element:
        return Web::AsyncScrollNodeKind::Element;
    case Compositing::CompositorScrollNodeKind::PseudoElement:
        return Web::AsyncScrollNodeKind::PseudoElement;
    }
    VERIFY_NOT_REACHED();
}

static Web::AsyncScrollNodeStableID stable_scroll_node_id_for(Web::UniqueNodeID scrollable_node_id, Compositing::CompositorScrollNodeKind kind, u8 pseudo_element_type)
{
    return {
        .node_id = scrollable_node_id,
        .kind = async_scroll_node_kind_for(kind),
        .pseudo_element_type = pseudo_element_type,
    };
}

static AsyncScrollNode async_scroll_node_from(auto const& node)
{
    Optional<Compositing::SpatialNodeIndex> parent_scroll_node_index;
    if (node.parent_scroll_node_index != Compositing::VISUAL_VIEWPORT_NODE_INDEX)
        parent_scroll_node_index = node.parent_scroll_node_index;
    return {
        .scroll_node_index = node.scroll_node_index,
        .stable_node_id = stable_scroll_node_id_for(node.scrollable_node_id, node.scroll_node_kind, node.pseudo_element_type),
        .parent_scroll_node_index = parent_scroll_node_index,
        .scrollport_rect = node.scrollport_rect,
        .min_scroll_offset = node.min_scroll_offset,
        .max_scroll_offset = node.max_scroll_offset,
        .is_viewport = node.is_viewport,
        .can_be_wheel_scrolled_horizontally = node.can_be_wheel_scrolled_horizontally,
        .can_be_wheel_scrolled_vertically = node.can_be_wheel_scrolled_vertically,
        .chains_scroll_to_ancestors_horizontally = node.chains_scroll_to_ancestors_horizontally,
        .chains_scroll_to_ancestors_vertically = node.chains_scroll_to_ancestors_vertically,
        .shows_overscroll_effect_horizontally = node.shows_overscroll_effect_horizontally,
        .shows_overscroll_effect_vertically = node.shows_overscroll_effect_vertically,
    };
}

static AsyncSnapContainer async_snap_container_from(auto const& container)
{
    return {
        .scroll_node_index = container.scroll_node_index,
        .geometry = {
            .snapport = container.snapport,
            .min_scroll_offset = container.min_scroll_offset,
            .max_scroll_offset = container.max_scroll_offset,
            .strictness = static_cast<SnapStrictness>(container.strictness),
            .axes = { .x = container.snaps_x, .y = container.snaps_y },
            .horizontal_writing_mode = container.horizontal_writing_mode,
        },
        .areas = {},
    };
}

static SnapAreaGeometry snap_area_geometry_from(auto const& area)
{
    return {
        .identity = { .node_id = area.area_node_id, .pseudo_element_type = area.pseudo_element_type },
        .rect = area.rect,
        .align_x = static_cast<SnapAlign>(area.align_x),
        .align_y = static_cast<SnapAlign>(area.align_y),
        .always_stop = area.always_stop,
    };
}

AsyncScrollingState async_scrolling_state_from(Compositing::DisplayList const& display_list, Compositing::AccumulatedVisualContextTree const& visual_context_tree)
{
    AsyncScrollingState async_scrolling_state;
    u32 next_paint_order_index = 0;

    if (auto const& metadata = display_list.async_scrolling_metadata(); metadata.has_value()) {
        async_scrolling_state.document_id = metadata->document_id;
        async_scrolling_state.viewport_rect = metadata->viewport_rect;
        async_scrolling_state.wheel_event_listener_state_generation = metadata->wheel_event_listener_state_generation;
        async_scrolling_state.has_blocking_wheel_event_listeners = metadata->has_blocking_wheel_event_listeners;
        async_scrolling_state.has_blocking_wheel_event_region_covering_viewport = metadata->has_blocking_wheel_event_region_covering_viewport;
        async_scrolling_state.viewport_keeps_horizontal_scrolls = metadata->viewport_keeps_horizontal_scrolls;
        async_scrolling_state.viewport_keeps_vertical_scrolls = metadata->viewport_keeps_vertical_scrolls;
        async_scrolling_state.device_pixels_per_css_pixel = metadata->device_pixels_per_css_pixel;
    }

    auto compositor_scroll_nodes = visual_context_tree.compositor_scroll_nodes();
    // Fall back to commands until WebContent fills the tree's compositor scroll table.
    bool use_scroll_table = !compositor_scroll_nodes.is_empty();
    if (use_scroll_table) {
        for (auto const& node : compositor_scroll_nodes)
            async_scrolling_state.scroll_nodes.append(async_scroll_node_from(node));
        auto snap_areas = visual_context_tree.compositor_snap_areas();
        for (auto const& container : visual_context_tree.compositor_snap_containers()) {
            auto snap_container = async_snap_container_from(container);
            for (auto const& area : snap_areas.slice(container.first_area_index, container.area_count))
                snap_container.areas.append(snap_area_geometry_from(area));
            async_scrolling_state.snap_containers.append(move(snap_container));
        }
    }

    display_list.for_each_compositor_metadata([&](Compositing::ContextRef context, Compositing::DisplayListCommandType command_type, ReadonlyBytes payload) {
        auto append_wheel_hit_test_target = [&](auto const& command, Gfx::CornerRadii corner_radii) {
            Optional<Compositing::SpatialNodeIndex> target_scroll_node_index;
            if (command.target_scroll_node_index.value())
                target_scroll_node_index = command.target_scroll_node_index;
            async_scrolling_state.wheel_hit_test_targets.append({
                .context = context,
                .rect = command.rect,
                .corner_radii = corner_radii,
                .target_scroll_node_index = target_scroll_node_index,
                .paint_order_index = next_paint_order_index++,
            });
        };

        switch (command_type) {
        case Compositing::DisplayListCommandType::CompositorBlockingWheelEventRegion: {
            auto command = Compositing::read_display_list_object<Compositing::CompositorBlockingWheelEventRegion>(payload);
            async_scrolling_state.has_blocking_wheel_event_listeners = true;
            async_scrolling_state.blocking_wheel_event_regions.append({
                .context = context,
                .rect = command.rect,
            });
            break;
        }
        case Compositing::DisplayListCommandType::CompositorScrollNode: {
            if (use_scroll_table)
                break;
            auto command = Compositing::read_display_list_object<Compositing::CompositorScrollNode>(payload);
            async_scrolling_state.scroll_nodes.append(async_scroll_node_from(command));
            break;
        }
        case Compositing::DisplayListCommandType::CompositorWheelHitTestTarget: {
            auto command = Compositing::read_display_list_object<Compositing::CompositorWheelHitTestTarget>(payload);
            append_wheel_hit_test_target(command, {});
            break;
        }
        case Compositing::DisplayListCommandType::CompositorWheelHitTestTargetWithCornerRadii: {
            auto command = Compositing::read_display_list_object<Compositing::CompositorWheelHitTestTargetWithCornerRadii>(payload);
            append_wheel_hit_test_target(command, command.corner_radii);
            break;
        }
        case Compositing::DisplayListCommandType::CompositorMainThreadWheelEventRegion: {
            auto command = Compositing::read_display_list_object<Compositing::CompositorMainThreadWheelEventRegion>(payload);
            async_scrolling_state.main_thread_wheel_event_regions.append({
                .context = context,
                .rect = command.rect,
            });
            break;
        }
        case Compositing::DisplayListCommandType::CompositorNestedContextWheelRegion: {
            auto command = Compositing::read_display_list_object<Compositing::CompositorNestedContextWheelRegion>(payload);
            async_scrolling_state.nested_context_wheel_regions.append({
                .context = context,
                .rect = command.rect,
                .corner_radii = command.corner_radii,
                .child_context_id = command.child_context_id,
                .paint_order_index = next_paint_order_index++,
            });
            break;
        }
        case Compositing::DisplayListCommandType::CompositorScrollbar: {
            auto command = Compositing::read_display_list_object<Compositing::CompositorScrollbar>(payload);
            async_scrolling_state.scrollbars.append({
                .scroller_stable_node_id = {},
                .scroll_node_index = command.scroll_node_index,
                .context = context,
                .paint_order_index = next_paint_order_index++,
                .gutter_rect = command.gutter_rect,
                .thumb_rect = command.thumb_rect,
                .track_rect = command.track_rect,
                .expanded_gutter_rect = command.expanded_gutter_rect,
                .expanded_thumb_rect = command.expanded_thumb_rect,
                .scroll_size = command.scroll_size,
                .expanded_scroll_size = command.expanded_scroll_size,
                .min_scroll_offset = command.min_scroll_offset,
                .max_scroll_offset = command.max_scroll_offset,
                .thumb_color = command.thumb_color,
                .thumb_outline_color = command.thumb_outline_color,
                .track_color = command.track_color,
                .vertical = command.vertical,
                .is_painted_by_compositor = command.is_painted_by_compositor,
            });
            break;
        }
        case Compositing::DisplayListCommandType::CompositorSnapContainer: {
            if (use_scroll_table)
                break;
            auto command = Compositing::read_display_list_object<Compositing::CompositorSnapContainer>(payload);
            async_scrolling_state.snap_containers.append(async_snap_container_from(command));
            break;
        }
        case Compositing::DisplayListCommandType::CompositorSnapArea: {
            if (use_scroll_table)
                break;
            auto command = Compositing::read_display_list_object<Compositing::CompositorSnapArea>(payload);
            auto& snap_containers = async_scrolling_state.snap_containers;
            // A snap container's areas are recorded right after it.
            if (snap_containers.is_empty() || snap_containers.last().scroll_node_index != command.scroll_node_index) {
                dbgln("Ignoring a snap area recorded without its snap container");
                break;
            }
            snap_containers.last().areas.append(snap_area_geometry_from(command));
            break;
        }
        default:
            break;
        }
    });

    for (auto& scrollbar : async_scrolling_state.scrollbars) {
        for (auto const& scroll_node : async_scrolling_state.scroll_nodes) {
            if (scroll_node.scroll_node_index == scrollbar.scroll_node_index) {
                scrollbar.scroller_stable_node_id = scroll_node.stable_node_id;
                break;
            }
        }
    }

    return async_scrolling_state;
}

static StringView snap_strictness_name(SnapStrictness strictness)
{
    switch (strictness) {
    case SnapStrictness::None:
        return "none"sv;
    case SnapStrictness::Proximity:
        return "proximity"sv;
    case SnapStrictness::Mandatory:
        return "mandatory"sv;
    }
    VERIFY_NOT_REACHED();
}

static StringView snap_align_name(SnapAlign align)
{
    switch (align) {
    case SnapAlign::None:
        return "none"sv;
    case SnapAlign::Start:
        return "start"sv;
    case SnapAlign::End:
        return "end"sv;
    case SnapAlign::Center:
        return "center"sv;
    }
    VERIFY_NOT_REACHED();
}

static double json_coordinate(Web::CSSPixels value) { return value.to_double(); }
static double json_coordinate(double value) { return value; }

static JsonObject rect_to_json(auto const& rect)
{
    JsonObject object;
    object.set("x"sv, json_coordinate(rect.x()));
    object.set("y"sv, json_coordinate(rect.y()));
    object.set("width"sv, json_coordinate(rect.width()));
    object.set("height"sv, json_coordinate(rect.height()));
    return object;
}

static JsonObject point_to_json(auto const& point)
{
    JsonObject object;
    object.set("x"sv, json_coordinate(point.x()));
    object.set("y"sv, json_coordinate(point.y()));
    return object;
}

JsonObject async_scrolling_state_to_json(AsyncScrollingState const& state)
{
    JsonArray scroll_nodes;
    for (auto const& scroll_node : state.scroll_nodes) {
        JsonObject node;
        node.set("scrollNodeIndex"sv, scroll_node.scroll_node_index.value());
        node.set("parentScrollNodeIndex"sv, scroll_node.parent_scroll_node_index.has_value() ? scroll_node.parent_scroll_node_index->value() : 0);
        node.set("isViewport"sv, scroll_node.is_viewport);
        node.set("scrollportRect"sv, rect_to_json(scroll_node.scrollport_rect));
        node.set("minScrollOffset"sv, point_to_json(scroll_node.min_scroll_offset));
        node.set("maxScrollOffset"sv, point_to_json(scroll_node.max_scroll_offset));
        scroll_nodes.must_append(move(node));
    }

    JsonArray snap_containers;
    for (auto const& snap_container : state.snap_containers) {
        JsonObject container;
        container.set("scrollNodeIndex"sv, snap_container.scroll_node_index.value());
        container.set("snapport"sv, rect_to_json(snap_container.geometry.snapport));
        container.set("minScrollOffset"sv, point_to_json(snap_container.geometry.min_scroll_offset));
        container.set("maxScrollOffset"sv, point_to_json(snap_container.geometry.max_scroll_offset));
        container.set("strictness"sv, snap_strictness_name(snap_container.geometry.strictness));
        container.set("snapsX"sv, snap_container.geometry.axes.x);
        container.set("snapsY"sv, snap_container.geometry.axes.y);
        container.set("horizontalWritingMode"sv, snap_container.geometry.horizontal_writing_mode);
        JsonArray areas;
        for (auto const& snap_area : snap_container.areas) {
            JsonObject area;
            area.set("nodeID"sv, snap_area.identity.node_id.value());
            area.set("pseudoElementType"sv, snap_area.identity.pseudo_element_type);
            area.set("rect"sv, rect_to_json(snap_area.rect));
            area.set("alignX"sv, snap_align_name(snap_area.align_x));
            area.set("alignY"sv, snap_align_name(snap_area.align_y));
            area.set("alwaysStop"sv, snap_area.always_stop);
            areas.must_append(move(area));
        }
        container.set("areas"sv, move(areas));
        snap_containers.must_append(move(container));
    }

    JsonObject object;
    object.set("documentID"sv, state.document_id.has_value() ? JsonValue { state.document_id->value() } : JsonValue {});
    object.set("scrollNodeCount"sv, state.scroll_nodes.size());
    object.set("scrollNodes"sv, move(scroll_nodes));
    object.set("snapContainers"sv, move(snap_containers));
    object.set("devicePixelsPerCssPixel"sv, state.device_pixels_per_css_pixel);
    object.set("hasBlockingWheelEventListeners"sv, state.has_blocking_wheel_event_listeners);
    object.set("blockingWheelEventRegionCount"sv, state.blocking_wheel_event_regions.size());
    object.set("mainThreadWheelEventRegionCount"sv, state.main_thread_wheel_event_regions.size());
    object.set("nestedContextWheelRegionCount"sv, state.nested_context_wheel_regions.size());
    object.set("wheelEventListenerStateGeneration"sv, state.wheel_event_listener_state_generation);
    object.set("hasBlockingWheelEventRegionCoveringViewport"sv, state.has_blocking_wheel_event_region_covering_viewport);
    return object;
}

WheelRoutingAdmission wheel_routing_admission_for(AsyncScrollingState const& state)
{
    if (state.has_blocking_wheel_event_region_covering_viewport)
        return WheelRoutingAdmission::BlockingWheelEventListeners;

    if (state.scroll_nodes.is_empty())
        return WheelRoutingAdmission::NoScrollNode;
    return WheelRoutingAdmission::Accepted;
}

Web::InputRoutingDecision input_routing_decision_for_wheel_step_not_admitted_by(WheelRoutingAdmission admission)
{
    switch (admission) {
    case WheelRoutingAdmission::Accepted:
        VERIFY_NOT_REACHED();
    case WheelRoutingAdmission::NoAsyncScrollingState:
        return Web::InputRoutingDecision::NoAsyncScrollingState;
    case WheelRoutingAdmission::BlockingWheelEventListeners:
        return Web::InputRoutingDecision::BlockingWheelEventListeners;
    case WheelRoutingAdmission::NoScrollNode:
        return Web::InputRoutingDecision::NoScrollNode;
    case WheelRoutingAdmission::StaleWheelEventListeners:
        return Web::InputRoutingDecision::StaleWheelEventListeners;
    }
    VERIFY_NOT_REACHED();
}

WheelRoutingAdmission wheel_routing_admission_for(AsyncScrollingState const& state, u64 newest_wheel_event_listener_state_generation)
{
    if (state.wheel_event_listener_state_generation < newest_wheel_event_listener_state_generation)
        return WheelRoutingAdmission::StaleWheelEventListeners;
    return wheel_routing_admission_for(state);
}

}

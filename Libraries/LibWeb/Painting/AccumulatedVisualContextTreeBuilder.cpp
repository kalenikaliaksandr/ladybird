/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 * Copyright (c) 2026, Jelle Raaijmakers <jelle@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/HashTable.h>
#include <LibCore/Environment.h>
#include <LibGfx/Matrix4x4.h>
#include <LibWeb/CSS/ComputedValues.h>
#include <LibWeb/CSS/StyleValues/TransformationStyleValue.h>
#include <LibWeb/CSS/VisualViewport.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/HTML/HTMLHtmlElement.h>
#include <LibWeb/Layout/Box.h>
#include <LibWeb/Page/Page.h>
#include <LibWeb/Painting/AccumulatedVisualContextTreeBuilder.h>
#include <LibWeb/Painting/Blending.h>
#include <LibWeb/Painting/DevicePixelConverter.h>
#include <LibWeb/Painting/Paintable.h>
#include <LibWeb/Painting/ResolvedCSSFilter.h>
#include <LibWeb/Painting/SVGForeignObjectPaintable.h>
#include <LibWeb/Painting/SVGGraphicsPaintable.h>
#include <LibWeb/Painting/SVGSVGPaintable.h>
#include <LibWeb/Painting/ScrollState.h>
#include <LibWeb/Painting/ViewportPaintable.h>

namespace Web::Painting {

static ScrollStateSlot common_ancestor_slot_along_scroll_parent_chain(ScrollState const& scroll_state, ScrollStateSlot a_slot, ScrollStateSlot b_slot)
{
    Vector<ScrollStateSlot, 8> a_slot_and_ancestors;
    for (auto slot = a_slot;; slot = scroll_state.state_at_slot(slot).parent_slot()) {
        a_slot_and_ancestors.append(slot);
        if (slot == NO_SCROLL_STATE_SLOT)
            break;
    }
    for (auto slot = b_slot;; slot = scroll_state.state_at_slot(slot).parent_slot()) {
        if (a_slot_and_ancestors.contains_slow(slot))
            return slot;
        if (slot == NO_SCROLL_STATE_SLOT)
            break;
    }
    return NO_SCROLL_STATE_SLOT;
}

static CSSPixelRect effective_css_clip_rect(CSSPixelRect const& css_clip)
{
    if (css_clip.width() < 0 || css_clip.height() < 0)
        return CSSPixelRect { 0, 0, 0, 0 };
    return css_clip;
}

// Converts a CSS-pixel-space 4x4 matrix to device-pixel-space.
// - Translation column (column 3, rows 0-2) is scaled up by DPR
// - Perspective row (row 3, columns 0-2) is scaled down by DPR
// - All other elements are unaffected (the scale factors cancel out)
static FloatMatrix4x4 scale_matrix_for_device_pixels(FloatMatrix4x4 matrix, float scale)
{
    matrix[0, 3] *= scale;
    matrix[1, 3] *= scale;
    matrix[2, 3] *= scale;
    matrix[3, 0] /= scale;
    matrix[3, 1] /= scale;
    matrix[3, 2] /= scale;
    return matrix;
}

static TransformData visual_viewport_transform_data(DOM::Document& document)
{
    auto scale = static_cast<float>(document.page().client().device_pixels_per_css_pixel());
    auto matrix = scale_matrix_for_device_pixels(document.visual_viewport()->transform().to_matrix(), scale);
    return TransformData { matrix, { 0.f, 0.f } };
}

static Optional<Gfx::AffineTransform> svg_to_css_pixels_transform(Paintable const& paintable)
{
    if (auto const* svg_graphics_paintable = as_if<SVGGraphicsPaintable>(paintable))
        return svg_graphics_paintable->computed_transforms().svg_to_css_pixels_transform();
    if (auto const* svg_foreign_object_paintable = as_if<SVGForeignObjectPaintable>(paintable))
        return svg_foreign_object_paintable->computed_transforms().svg_to_css_pixels_transform();
    if (auto const* svg_svg_paintable = as_if<SVGSVGPaintable>(paintable))
        return svg_svg_paintable->computed_transforms().svg_to_css_pixels_transform();
    return {};
}

static bool computed_values_have_transform(CSS::ComputedValues const& computed_values)
{
    return !computed_values.transformations().is_empty()
        || !computed_values.rotate().is_null()
        || !computed_values.translate().is_null()
        || !computed_values.scale().is_null();
}

// https://drafts.csswg.org/css-transforms-2/#ctm
Optional<TransformData> compute_transform(Paintable const& paintable_box, CSS::ComputedValues const& computed_values, double pixel_ratio)
{
    if (!computed_values_have_transform(computed_values) || !paintable_box.layout_node().is_transformable())
        return {};

    // The transformation matrix is computed from the transform, transform-origin, translate, rotate, scale, and
    // offset properties as follows:
    auto reference_box = paintable_box.transform_reference_box();
    auto const& css_transform_origin = computed_values.transform_origin();
    auto origin_x = css_transform_origin.x.to_px(reference_box.width());
    auto origin_y = css_transform_origin.y.to_px(reference_box.height());
    auto origin_z = css_transform_origin.z.to_px(0).to_float();

    // 1. Start with the identity matrix.
    // 2. Translate by the computed X, Y, and Z values of transform-origin.
    auto matrix = Gfx::translation_matrix(Vector3 { 0.f, 0.f, origin_z });

    // 3. Translate by the computed X, Y, and Z values of translate.
    if (auto const& translate = computed_values.translate())
        matrix = matrix * translate->to_matrix(paintable_box);

    // 4. Rotate by the computed <angle> about the specified axis of rotate.
    if (auto const& rotate = computed_values.rotate())
        matrix = matrix * rotate->to_matrix(paintable_box);

    // 5. Scale by the computed X, Y, and Z values of scale.
    if (auto const& scale = computed_values.scale())
        matrix = matrix * scale->to_matrix(paintable_box);

    // FIXME: 6. Translate and rotate by the transform specified by offset.

    // 7. Multiply by each of the transform functions in transform from left to right.
    for (auto const& transform : computed_values.transformations())
        matrix = matrix * transform->to_matrix(paintable_box);

    // 8. Translate by the negated computed X, Y and Z values of transform-origin.
    matrix = matrix * Gfx::translation_matrix(Vector3 { 0.f, 0.f, -origin_z });

    // https://svgwg.org/svg2-draft/coords.html#ViewBoxAttribute
    // The presence of the viewBox attribute results in a transformation being applied to the viewport coordinate system
    if (auto svg_to_css_pixels = svg_to_css_pixels_transform(paintable_box); svg_to_css_pixels.has_value() && !svg_to_css_pixels->is_identity()) {
        if (auto inverse_svg_to_css_pixels = svg_to_css_pixels->inverse(); inverse_svg_to_css_pixels.has_value())
            matrix = svg_to_css_pixels->to_matrix() * matrix * inverse_svg_to_css_pixels->to_matrix();
    }

    auto origin = reference_box.location() + CSSPixelPoint { origin_x, origin_y };
    auto scale = static_cast<float>(pixel_ratio);
    auto device_origin = origin.to_type<float>() * scale;
    return TransformData { scale_matrix_for_device_pixels(matrix, scale), device_origin };
}

// https://drafts.csswg.org/css-transforms-2/#perspective-matrix
static Optional<Gfx::FloatMatrix4x4> compute_perspective_matrix(Paintable const& paintable_box, CSS::ComputedValues const& computed_values)
{
    auto perspective = computed_values.perspective();
    if (!perspective.has_value() || !paintable_box.layout_node().is_transformable())
        return {};

    // The perspective matrix is computed as follows:

    // 1. Start with the identity matrix.
    // 2. Translate by the computed X and Y values of 'perspective-origin'
    // https://drafts.csswg.org/css-transforms-2/#perspective-origin-property
    // Percentages: refer to the size of the reference box
    auto reference_box = paintable_box.transform_reference_box();
    auto perspective_origin = computed_values.perspective_origin().resolved(reference_box);
    auto computed_x = perspective_origin.x().to_float();
    auto computed_y = perspective_origin.y().to_float();
    auto perspective_matrix = Gfx::translation_matrix(Vector3<float>(computed_x, computed_y, 0));

    // 3. Multiply by the matrix that would be obtained from the 'perspective()' transform function, where the
    //    length is provided by the value of the perspective property
    // https://drafts.csswg.org/css-transforms-2/#funcdef-perspective
    // If the depth value is less than '1px', it must be treated as '1px' for the purpose of rendering, [..]
    auto distance = max(perspective->to_float(), 1.f);
    perspective_matrix = perspective_matrix * Gfx::perspective_matrix(distance);

    // 4. Translate by the negated computed X and Y values of 'perspective-origin'
    return perspective_matrix * Gfx::translation_matrix(Vector3 { -computed_x, -computed_y, 0.f });
}

static Optional<ClipData> compute_clip_data(Paintable const& paintable_box, CSS::ComputedValues const& computed_values, DevicePixelConverter const& converter)
{
    auto overflow_x = computed_values.overflow_x();
    auto overflow_y = computed_values.overflow_y();

    // https://drafts.csswg.org/css-contain-2/#paint-containment
    // 1. The contents of the element including any ink or scrollable overflow must be clipped to the overflow clip
    //    edge of the paint containment box, taking corner clipping into account. This does not include the creation of
    //    any mechanism to access or indicate the presence of the clipped content; nor does it inhibit the creation of
    //    any such mechanism through other properties, such as overflow, resize, or text-overflow.
    //    NOTE: This clipping shape respects overflow-clip-margin, allowing an element with paint containment
    //          to still slightly overflow its normal bounds.
    auto has_paint_containment = computed_values.contain().paint_containment
        || computed_values.content_visibility() == CSS::ContentVisibility::Auto;
    if (has_paint_containment && paintable_box.layout_node().has_paint_containment()) {
        // NOTE: Note: The behavior is described in this paragraph is equivalent to changing 'overflow-x: visible' into
        //       'overflow-x: clip' and 'overflow-y: visible' into 'overflow-y: clip' at used value time, while leaving other
        //       values of 'overflow-x' and 'overflow-y' unchanged.
        overflow_x = CSS::Overflow::Clip;
        overflow_y = CSS::Overflow::Clip;
    }

    auto has_hidden_overflow = overflow_x != CSS::Overflow::Visible || overflow_y != CSS::Overflow::Visible;

    if (has_hidden_overflow && paintable_box.overflow_property_applies()) {
        auto clip_rect = paintable_box.absolute_padding_box_rect();

        // https://drafts.csswg.org/css-overflow-3/#propdef-overflow
        // 'clip'
        //    This value indicates that the box’s content is clipped to its overflow clip edge
        auto overflow_clip_edge = paintable_box.overflow_clip_edge_rect();
        if (overflow_x == CSS::Overflow::Visible) {
            clip_rect.set_left(0);
            clip_rect.set_right(CSSPixels::max_integer_value);
        } else if (overflow_x == CSS::Overflow::Clip) {
            clip_rect.set_left(overflow_clip_edge.left());
            clip_rect.set_right(overflow_clip_edge.right());
        }
        if (overflow_y == CSS::Overflow::Visible) {
            clip_rect.set_top(0);
            clip_rect.set_bottom(CSSPixels::max_integer_value);
        } else if (overflow_y == CSS::Overflow::Clip) {
            clip_rect.set_top(overflow_clip_edge.top());
            clip_rect.set_bottom(overflow_clip_edge.bottom());
        }

        // https://drafts.csswg.org/css-overflow-3/#corner-clipping
        // As mentioned in CSS Backgrounds 3 § 4.3 Corner Clipping, the clipping region established by 'overflow' can be
        // rounded:
        // - When 'overflow-x' and 'overflow-y' compute to 'hidden', 'scroll', or 'auto', the clipping region is rounded
        //   based on the border radius, adjusted to the padding edge, as described in CSS Backgrounds 3 § 4.2 Corner
        //   Shaping.
        // - When both 'overflow-x' and 'overflow-y' compute to 'clip', the clipping region is rounded as described in § 3.2
        //   Expanding Clipping Bounds: the 'overflow-clip-margin' property.
        // - However, when one of 'overflow-x' or 'overflow-y' computes to 'clip' and the other computes to 'visible', the
        //   clipping region is not rounded.
        // FIXME: Adjust the border radii for the overflow-clip-margin case. (see https://drafts.csswg.org/css-overflow-4/#valdef-overflow-clip-margin-length-0 )
        auto radii = (overflow_x != CSS::Overflow::Visible && overflow_y != CSS::Overflow::Visible) ? paintable_box.normalized_border_radii_data(Paintable::ShrinkRadiiForBorders::Yes) : BorderRadiiData {};
        return ClipData { converter.rounded_device_rect(clip_rect), radii.as_corners(converter) };
    }

    return {};
}

static Optional<ClipData> compute_css_clip_data(Paintable const& paintable_box, CSS::ComputedValues const& computed_values, DevicePixelConverter const& converter)
{
    if (!computed_values.clip().is_rect())
        return {};
    if (auto css_clip = paintable_box.get_clip_rect(); css_clip.has_value()) {
        auto effective_rect = effective_css_clip_rect(*css_clip);
        return ClipData { converter.rounded_device_rect(effective_rect), {} };
    }
    return {};
}

static Optional<ClipPathData> compute_basic_shape_clip_path_data(Paintable const& paintable_box, CSS::ComputedValues const& computed_values, DevicePixelConverter const& converter, float scale)
{
    // FIXME: Support other geometry boxes. See: https://drafts.fxtf.org/css-masking/#typedef-geometry-box
    auto const& clip_path = computed_values.clip_path();
    if (!clip_path.has_value() || !clip_path->is_basic_shape())
        return {};

    auto masking_area = paintable_box.absolute_border_box_rect();
    auto reference_box = CSSPixelRect { {}, masking_area.size() };
    auto const& basic_shape = clip_path->basic_shape();
    auto path = basic_shape.to_path(reference_box);
    path.offset(masking_area.top_left().template to_type<float>());
    auto fill_rule = basic_shape.basic_shape().visit(
        [](CSS::Polygon const& polygon) { return polygon.fill_rule; },
        [](CSS::Path const& path) { return path.fill_rule; },
        [](auto const&) { return Gfx::WindingRule::Nonzero; });
    auto device_path = path.copy_transformed(Gfx::AffineTransform {}.set_scale(scale, scale));
    auto device_bounding_rect = converter.rounded_device_rect(masking_area);
    return ClipPathData { move(device_path), device_bounding_rect, fill_rule };
}

static Optional<PerspectiveData> compute_perspective_data(Paintable const& paintable_box, CSS::ComputedValues const& computed_values, float scale)
{
    auto perspective_matrix = compute_perspective_matrix(paintable_box, computed_values);
    if (!perspective_matrix.has_value())
        return {};
    return PerspectiveData { scale_matrix_for_device_pixels(*perspective_matrix, scale) };
}

// NB: Resolves the box's filter as a side effect, since the effects data embeds the resolved gfx filter.
static Optional<EffectsData> compute_effects_data(Paintable& box, CSS::ComputedValues const& computed_values, double pixel_ratio)
{
    if (computed_values.filter().has_filters())
        box.set_filter(resolve_css_filter(computed_values.filter(), box));
    else if (box.filter().has_filters() || box.filter().svg_filter_bounds.has_value())
        box.set_filter({});

    if (!box.filter().has_filters() && computed_values.opacity() == 1 && computed_values.mix_blend_mode() == CSS::MixBlendMode::Normal)
        return {};

    Optional<Gfx::Filter> gfx_filter;
    if (box.filter().has_filters())
        gfx_filter = to_gfx_filter(box.filter(), pixel_ratio);
    EffectsData effects {
        computed_values.opacity(),
        mix_blend_mode_to_compositing_and_blending_operator(computed_values.mix_blend_mode()),
        move(gfx_filter)
    };
    if (!effects.needs_layer())
        return {};
    return effects;
}

// Nearest ancestor scroll node resolved along the containing block chain, drilled down alongside
// the visual context indices. A fixed-position ancestor decouples its subtree from all outer
// scrollers, but sticky boxes must still reference a scrollport through fixed-position ancestors
// for their sticky offset computation, so both resolutions are carried.
struct NearestScrollNodeIndices {
    VisualContextIndex stopping_at_fixed_position_ancestors;
    VisualContextIndex continuing_through_fixed_position_ancestors;
};

struct DescendantVisualContexts {
    VisualContextIndex normal;
    VisualContextIndex absolute_position;
    VisualContextIndex fixed_position;
    NearestScrollNodeIndices normal_nearest_scroll_nodes;
    NearestScrollNodeIndices absolute_position_nearest_scroll_nodes;
    NearestScrollNodeIndices fixed_position_nearest_scroll_nodes;
};

// Builds visual context nodes for whole paintable subtrees, seeded with the contexts the subtree
// root inherits. One builder spans one build operation, so anchor-positioned subtree deferral
// works across every subtree it builds. In reconcile mode the walk runs over a tree that already
// describes these paintables: each box's recorded nodes are consumed by matching fresh emissions,
// which patch surviving nodes at their slots; only divergences allocate or free.
class VisualContextTreeBuilder {
public:
    enum class Mode {
        Build,
        ReconcileInPlace,
    };

    VisualContextTreeBuilder(ViewportPaintable& viewport_paintable, AccumulatedVisualContextTree& visual_context_tree, Mode mode)
        : m_viewport_paintable(viewport_paintable)
        , m_visual_context_tree(visual_context_tree)
        , m_reconcile_in_place(mode == Mode::ReconcileInPlace)
        , m_pixel_ratio(viewport_paintable.document().page().client().device_pixels_per_css_pixel())
        , m_converter(m_pixel_ratio)
        , m_scale(static_cast<float>(m_pixel_ratio))
    {
    }

    VisualContextReconcileOutcome reconcile_outcome() const { return m_outcome; }

    void build_subtree(Paintable& root, DescendantVisualContexts inherited_contexts, bool may_be_root_element)
    {
        m_pending_paintables.clear_with_capacity();
        m_pending_paintables.append({ &root, inherited_contexts, may_be_root_element });
        build_paintables_deferring_anchor_positioned(nullptr);
    }

    void build_deferred_anchor_positioned_subtrees()
    {
        while (!m_deferred_anchor_positioned_paintables.is_empty()) {
            auto entries = move(m_deferred_anchor_positioned_paintables);
            Vector<PendingPaintable> entries_whose_anchor_is_still_deferred;
            for (auto& entry : entries) {
                if (anchor_is_awaiting_build(*entry.paintable))
                    entries_whose_anchor_is_still_deferred.append(entry);
                else
                    build_deferred_subtree(entry);
            }

            bool no_entry_was_ready = entries_whose_anchor_is_still_deferred.size() == entries.size();
            if (no_entry_was_ready) {
                // Cyclic or otherwise malformed anchor chains can leave every remaining entry waiting on another;
                // build them in queue order then — the anchor chain walk's visited set and depth cap bound the
                // damage the same way they do for cycles discovered mid-walk.
                for (auto& entry : entries_whose_anchor_is_still_deferred)
                    build_deferred_subtree(entry);
            } else {
                m_deferred_anchor_positioned_paintables.extend(move(entries_whose_anchor_is_still_deferred));
            }
        }
    }

private:
    struct PendingPaintable {
        Paintable* paintable;
        DescendantVisualContexts inherited_contexts;
        bool may_be_root_element;
    };

    VisualContextIndex append_node(VisualContextIndex parent_index, VisualContextData data)
    {
        if (m_reconcile_in_place && m_reuse_cursor_position < m_reusable_nodes_of_current_paintable.size()) {
            auto index = m_reusable_nodes_of_current_paintable[m_reuse_cursor_position];
            auto& node = m_visual_context_tree.node_at(index);
            bool same_alternative = node.data.visit([&]<typename DataType>(DataType const&) { return data.has<DataType>(); });
            if (same_alternative) {
                if (auto const* old_scroll = node.data.get_pointer<ScrollData>(); old_scroll && old_scroll->is_sticky != data.get<ScrollData>().is_sticky)
                    same_alternative = false;
            }
            if (same_alternative) {
                ++m_reuse_cursor_position;
                if (auto const* old_scroll = node.data.get_pointer<ScrollData>())
                    data.get<ScrollData>().state_slot = old_scroll->state_slot;
                auto patch_result = m_visual_context_tree.patch_node(index, move(data), parent_index);
                if (patch_result.parent_or_depth_changed || patch_result.empty_effective_clip_flipped)
                    m_outcome.structural_change = true;
                m_emitted_nodes_for_current_paintable.append(index);
                return index;
            }
            free_remaining_reusable_nodes();
        }
        if (m_reconcile_in_place)
            m_outcome.structural_change = true;
        auto index = m_visual_context_tree.allocate_node(move(data), parent_index);
        m_emitted_nodes_for_current_paintable.append(index);
        return index;
    }

    void free_remaining_reusable_nodes()
    {
        while (m_reuse_cursor_position < m_reusable_nodes_of_current_paintable.size()) {
            m_viewport_paintable.free_visual_context_node(m_visual_context_tree, m_reusable_nodes_of_current_paintable[m_reuse_cursor_position++]);
            m_outcome.structural_change = true;
        }
    }

    enum class StickyNode {
        No,
        Yes,
    };

    void register_or_update_scroll_node(VisualContextIndex node_index, Paintable const& paintable_box, VisualContextIndex parent_index, StickyNode sticky)
    {
        // A patched node kept its state slot; a freshly allocated one still carries the sentinel.
        if (m_visual_context_tree.scroll_state_slot_for_node(node_index) != NO_SCROLL_STATE_SLOT) {
            m_viewport_paintable.update_scroll_node_state_keeping_slot(m_visual_context_tree, node_index, paintable_box, parent_index);
            return;
        }
        if (sticky == StickyNode::Yes)
            m_viewport_paintable.register_sticky_node(m_visual_context_tree, node_index, paintable_box, parent_index);
        else
            m_viewport_paintable.register_scroll_node(m_visual_context_tree, node_index, paintable_box, parent_index);
    }

    static bool has_default_scroll_shift_anchor(Paintable const& paintable_box)
    {
        auto const* box = as_if<Layout::Box>(paintable_box.layout_node());
        return box && box->default_scroll_shift_anchor();
    }

    void build_paintables_deferring_anchor_positioned(Paintable const* paintable_exempt_from_deferral)
    {
        while (!m_pending_paintables.is_empty()) {
            auto pending = m_pending_paintables.take_last();
            if (pending.paintable != paintable_exempt_from_deferral && has_default_scroll_shift_anchor(*pending.paintable)) {
                m_deferred_anchor_positioned_paintables.append(pending);
                m_deferred_paintables_awaiting_build.set(pending.paintable);
                continue;
            }
            auto child_contexts = build_paintable_box(*pending.paintable, pending.inherited_contexts, pending.may_be_root_element);
            for (auto* child = pending.paintable->last_child_ptr(); child; child = child->previous_sibling_ptr())
                m_pending_paintables.append({ child, child_contexts, false });
        }
    }

    bool anchor_is_awaiting_build(Paintable const& paintable_box) const
    {
        auto const* box = as_if<Layout::Box>(paintable_box.layout_node());
        auto const* anchor_box = box ? as_if<Layout::Box>(box->default_scroll_shift_anchor()) : nullptr;
        auto anchor_paintable = anchor_box ? anchor_box->paintable_box() : nullptr;
        for (auto const* paintable = anchor_paintable.ptr(); paintable; paintable = paintable->parent_ptr()) {
            if (m_deferred_paintables_awaiting_build.contains(paintable))
                return true;
        }
        return false;
    }

    void build_deferred_subtree(PendingPaintable& entry)
    {
        m_deferred_paintables_awaiting_build.remove(entry.paintable);
        m_pending_paintables.clear_with_capacity();
        m_pending_paintables.append(entry);
        build_paintables_deferring_anchor_positioned(entry.paintable);
    }

    DescendantVisualContexts build_paintable_box(Paintable& paintable_box, DescendantVisualContexts inherited_contexts, bool may_be_root_element)
    {
        m_emitted_nodes_for_current_paintable.clear_with_capacity();
        // A fresh build discards the old tree wholesale, so only a reconcile may interpret the
        // recorded nodes: they index the tree being reconciled, and freeing them anywhere else
        // would tombstone whatever occupies those slots in the new tree.
        if (m_reconcile_in_place) {
            m_reusable_nodes_of_current_paintable.clear_with_capacity();
            m_reusable_nodes_of_current_paintable.append(paintable_box.owned_visual_context_nodes().data(), paintable_box.owned_visual_context_nodes().size());
            m_reuse_cursor_position = 0;
        }
        auto& layout_node = paintable_box.layout_node();

        paintable_box.set_enclosing_scroll_node_index({});
        paintable_box.set_own_scroll_node_index({});

        auto nearest_scroll_nodes_for_descendants = [&]() -> NearestScrollNodeIndices {
            if (paintable_box.is_fixed_position())
                return { {}, inherited_contexts.fixed_position_nearest_scroll_nodes.continuing_through_fixed_position_ancestors };
            if (paintable_box.is_absolutely_positioned())
                return inherited_contexts.absolute_position_nearest_scroll_nodes;
            return inherited_contexts.normal_nearest_scroll_nodes;
        }();
        auto nearest_ancestor_scroll_node_index = paintable_box.is_sticky_position()
            ? nearest_scroll_nodes_for_descendants.continuing_through_fixed_position_ancestors
            : nearest_scroll_nodes_for_descendants.stopping_at_fixed_position_ancestors;
        if (!paintable_box.is_fixed_position() && !paintable_box.is_sticky_position())
            paintable_box.set_enclosing_scroll_node_index(nearest_ancestor_scroll_node_index);

        bool creates_sticky_scroll_node = paintable_box.is_sticky_position() && paintable_box.has_sticky_insets();

        VisualContextIndex inherited_state;

        if (paintable_box.is_fixed_position()) {
            inherited_state = inherited_contexts.fixed_position;
        } else if (paintable_box.is_absolutely_positioned()) {
            inherited_state = inherited_contexts.absolute_position;
        } else {
            // In-flow and relatively positioned boxes inherit the normal descendant context from their visual parent.
            inherited_state = inherited_contexts.normal;
        }

        // Build this element's own state from inherited state.
        VisualContextIndex own_state = inherited_state;

        // https://drafts.csswg.org/css-anchor-position-1/#default-scroll-shift
        // After layout has been performed for abspos, it is additionally shifted by the default scroll shift, as if
        // affected by a transform (before any other transforms).
        // NB: The shift is the scroll movement of the frames between the box's containing block and its default anchor
        //     box. When the anchor is itself an anchor-positioned box, its layout position does not include its own
        //     paint-time shift, so each chained anchor's shift is emitted as well, masked to the axes that every link
        //     below it compensates in. The visited set and depth cap guard against malformed anchor chains.
        if (auto const* box = as_if<Layout::Box>(&layout_node)) {
            auto const& scroll_state = m_viewport_paintable.scroll_state();
            bool compensate_horizontal_scroll = true;
            bool compensate_vertical_scroll = true;
            Vector<Layout::Box const*, 8> visited;
            constexpr size_t max_anchor_chain_depth = 32;
            while (box && !visited.contains_slow(box) && visited.size() < max_anchor_chain_depth) {
                auto const* anchor_box = as_if<Layout::Box>(box->default_scroll_shift_anchor());
                if (!anchor_box)
                    break;
                auto box_paintable = box->paintable_box();
                auto anchor_paintable = anchor_box->paintable_box();
                if (!box_paintable || !anchor_paintable)
                    break;
                visited.append(box);
                compensate_horizontal_scroll = compensate_horizontal_scroll && box->compensates_for_horizontal_scroll();
                compensate_vertical_scroll = compensate_vertical_scroll && box->compensates_for_vertical_scroll();
                auto anchor_scroll_slot = m_visual_context_tree.scroll_state_slot_for_node(anchor_paintable->enclosing_scroll_node_index());
                auto base_scroll_slot = m_visual_context_tree.scroll_state_slot_for_node(box_paintable->enclosing_scroll_node_index());
                auto shared_scroll_slot = common_ancestor_slot_along_scroll_parent_chain(scroll_state, anchor_scroll_slot, base_scroll_slot);
                for (auto slot = anchor_scroll_slot; slot != NO_SCROLL_STATE_SLOT && slot != shared_scroll_slot; slot = scroll_state.state_at_slot(slot).parent_slot())
                    own_state = append_node(own_state, AnchorScrollShift { scroll_state.node_index_for_slot(slot), false, compensate_horizontal_scroll, compensate_vertical_scroll });
                for (auto slot = base_scroll_slot; slot != NO_SCROLL_STATE_SLOT && slot != shared_scroll_slot; slot = scroll_state.state_at_slot(slot).parent_slot())
                    own_state = append_node(own_state, AnchorScrollShift { scroll_state.node_index_for_slot(slot), true, compensate_horizontal_scroll, compensate_vertical_scroll });
                box = anchor_box;
            }
        }

        // Out-of-flow descendants can skip overflow and scroll clips from intermediate ancestors. Keep their visual
        // contexts separate as we descend, and replace them with the normal descendant context only when this box
        // establishes the relevant containing block. A chain this box replaces below gets no copies appended:
        // they would be orphaned nodes that nothing in the built tree ever references.
        auto positioning_containing_blocks = layout_node.establishes_positioning_containing_blocks();
        VisualContextIndex state_for_absolute_position_descendants = inherited_contexts.absolute_position;
        VisualContextIndex state_for_fixed_position_descendants = inherited_contexts.fixed_position;

        auto append_to_own_and_positioned_descendant_contexts = [&](auto const& data) {
            own_state = append_node(own_state, data);
            if (!positioning_containing_blocks.absolute)
                state_for_absolute_position_descendants = append_node(state_for_absolute_position_descendants, data);
            if (!positioning_containing_blocks.fixed)
                state_for_fixed_position_descendants = append_node(state_for_fixed_position_descendants, data);
        };

        VisualContextIndex sticky_scroll_node_index;
        if (creates_sticky_scroll_node) {
            sticky_scroll_node_index = append_node(own_state, ScrollData { .is_sticky = true });
            own_state = sticky_scroll_node_index;
            register_or_update_scroll_node(sticky_scroll_node_index, paintable_box, nearest_ancestor_scroll_node_index, StickyNode::Yes);
            paintable_box.set_enclosing_scroll_node_index(sticky_scroll_node_index);
            paintable_box.set_own_scroll_node_index(sticky_scroll_node_index);
            nearest_scroll_nodes_for_descendants = { sticky_scroll_node_index, sticky_scroll_node_index };
        }

        auto const& computed_values = layout_node.computed_values();

        if (auto effects = compute_effects_data(paintable_box, computed_values, m_pixel_ratio); effects.has_value())
            append_to_own_and_positioned_descendant_contexts(effects.value());

        if (computed_values_have_transform(computed_values)) {
            if (auto transform_data = compute_transform(paintable_box, computed_values, m_pixel_ratio); transform_data.has_value()) {
                paintable_box.set_has_non_invertible_css_transform(!transform_data->matrix.is_invertible());
                own_state = append_node(own_state, *transform_data);
            } else {
                paintable_box.set_has_non_invertible_css_transform(false);
            }
        } else {
            paintable_box.set_has_non_invertible_css_transform(false);
        }

        if (computed_values.clip().is_rect()) {
            if (auto css_clip = compute_css_clip_data(paintable_box, computed_values, m_converter); css_clip.has_value())
                append_to_own_and_positioned_descendant_contexts(css_clip.value());
        }

        if (auto clip_path_data = compute_basic_shape_clip_path_data(paintable_box, computed_values, m_converter, m_scale); clip_path_data.has_value())
            append_to_own_and_positioned_descendant_contexts(clip_path_data.value());

        for (auto const& mask_layer : paintable_box.mask_layer_presence(MaskLayerSet::CssAndSvg))
            append_to_own_and_positioned_descendant_contexts(MaskData { m_converter.enclosing_device_rect(mask_layer.area), mask_layer.kind, mask_layer.origin });

        paintable_box.set_accumulated_visual_context(own_state);
        paintable_box.clear_fixed_background_visual_context();

        Vector<CSS::BackgroundLayerData> const* background_layers = &computed_values.background_layers();
        auto is_root_element = may_be_root_element && layout_node.is_root_element();
        if (is_root_element) {
            if (auto* html_element = as_if<HTML::HTMLHtmlElement>(paintable_box.dom_node().ptr())) {
                if (html_element->should_use_body_background_properties())
                    background_layers = paintable_box.document().background_layers();
            }
        }

        if (background_layers) {
            bool has_fixed_background = false;
            for (auto const& layer : *background_layers) {
                if (layer.background_image && layer.attachment == CSS::BackgroundAttachment::Fixed) {
                    has_fixed_background = true;
                    break;
                }
            }

            if (has_fixed_background) {
                // https://drafts.csswg.org/css-transforms-1/#transform-rendering
                // For elements that are effected by a transform (i.e. have a transform applied to them, or to any of
                // their ancestor elements) and do not have their background propagated to the canvas, a value of fixed
                // for the background-attachment property is treated as if it had a value of scroll.
                auto has_transform_ancestor = false;
                if (!is_root_element) {
                    for (Layout::NodeWithStyle const* node = &layout_node; node && !node->is_viewport(); node = node->parent()) {
                        if (node->has_css_transform()) {
                            has_transform_ancestor = true;
                            break;
                        }
                    }
                }

                if (!has_transform_ancestor) {
                    // Build a context that negates all scroll nodes in the ancestor chain. This keeps the background
                    // fixed relative to the viewport.
                    auto fixed_background_context = own_state;
                    for (auto index = own_state; index.value(); index = m_visual_context_tree.node_at(index).parent_index) {
                        auto const& node = m_visual_context_tree.node_at(index);
                        if (node.data.has<ScrollData>())
                            fixed_background_context = append_node(fixed_background_context, ScrollCompensation { index });
                    }
                    paintable_box.set_fixed_background_visual_context(fixed_background_context);
                }
            }
        }

        // Build state for descendants: own state + perspective + clip + scroll.
        VisualContextIndex state_for_descendants = own_state;

        if (computed_values.perspective().has_value()) {
            if (auto perspective_data = compute_perspective_data(paintable_box, computed_values, m_scale); perspective_data.has_value())
                state_for_descendants = append_node(state_for_descendants, *perspective_data);
        }

        auto may_have_clip = computed_values.overflow_x() != CSS::Overflow::Visible
            || computed_values.overflow_y() != CSS::Overflow::Visible
            || computed_values.contain().paint_containment
            || computed_values.content_visibility() == CSS::ContentVisibility::Auto;
        if (may_have_clip) {
            if (auto clip_data = compute_clip_data(paintable_box, computed_values, m_converter); clip_data.has_value())
                state_for_descendants = append_node(state_for_descendants, clip_data.value());
        }

        if (paintable_box.has_scrollable_overflow()) {
            auto parent_index = creates_sticky_scroll_node ? sticky_scroll_node_index : nearest_ancestor_scroll_node_index;
            auto scroll_node_index = append_node(state_for_descendants, ScrollData { .is_sticky = false });
            state_for_descendants = scroll_node_index;
            register_or_update_scroll_node(scroll_node_index, paintable_box, parent_index, StickyNode::No);
            paintable_box.set_own_scroll_node_index(scroll_node_index);
            nearest_scroll_nodes_for_descendants = { scroll_node_index, scroll_node_index };
        }

        if (m_reconcile_in_place)
            free_remaining_reusable_nodes();

        paintable_box.set_accumulated_visual_context_for_descendants(state_for_descendants);
        paintable_box.set_owned_visual_context_nodes(m_emitted_nodes_for_current_paintable, m_visual_context_tree.identity());
        auto absolute_position_nearest_scroll_nodes = inherited_contexts.absolute_position_nearest_scroll_nodes;
        auto fixed_position_nearest_scroll_nodes = inherited_contexts.fixed_position_nearest_scroll_nodes;
        if (positioning_containing_blocks.absolute) {
            state_for_absolute_position_descendants = state_for_descendants;
            absolute_position_nearest_scroll_nodes = nearest_scroll_nodes_for_descendants;
        }
        if (positioning_containing_blocks.fixed) {
            state_for_fixed_position_descendants = state_for_descendants;
            fixed_position_nearest_scroll_nodes = nearest_scroll_nodes_for_descendants;
        }

        paintable_box.set_accumulated_visual_context_for_absolute_position_descendants(state_for_absolute_position_descendants);
        paintable_box.set_accumulated_visual_context_for_fixed_position_descendants(state_for_fixed_position_descendants);

        return DescendantVisualContexts {
            state_for_descendants,
            state_for_absolute_position_descendants,
            state_for_fixed_position_descendants,
            nearest_scroll_nodes_for_descendants,
            absolute_position_nearest_scroll_nodes,
            fixed_position_nearest_scroll_nodes,
        };
    }

    ViewportPaintable& m_viewport_paintable;
    AccumulatedVisualContextTree& m_visual_context_tree;
    bool m_reconcile_in_place { false };
    VisualContextReconcileOutcome m_outcome;
    double m_pixel_ratio { 0 };
    DevicePixelConverter m_converter;
    float m_scale { 0 };

    // Anchor-positioned boxes emit AnchorScrollShift nodes by reading the enclosing scroll nodes of their
    // anchors, and an acceptable anchor may come later in tree order than the positioned box. Building such
    // boxes' subtrees is deferred until their anchors have been built; the hash table mirrors the queue so
    // readiness checks stay cheap across rounds.
    Vector<PendingPaintable> m_deferred_anchor_positioned_paintables;
    HashTable<Paintable const*> m_deferred_paintables_awaiting_build;

    Vector<PendingPaintable, 64> m_pending_paintables;
    Vector<VisualContextIndex> m_emitted_nodes_for_current_paintable;
    Vector<VisualContextIndex> m_reusable_nodes_of_current_paintable;
    size_t m_reuse_cursor_position { 0 };
};

static VisualContextReconcileOutcome stamp_viewport_and_build_subtrees(ViewportPaintable& viewport_paintable, AccumulatedVisualContextTree& visual_context_tree, VisualContextIndex viewport_state_for_descendants, VisualContextTreeBuilder::Mode mode)
{
    viewport_paintable.set_enclosing_scroll_node_index({});
    viewport_paintable.set_own_scroll_node_index(viewport_state_for_descendants);
    viewport_paintable.set_accumulated_visual_context(VISUAL_VIEWPORT_NODE_INDEX);
    viewport_paintable.set_accumulated_visual_context_for_descendants(viewport_state_for_descendants);
    viewport_paintable.set_accumulated_visual_context_for_absolute_position_descendants(viewport_state_for_descendants);
    viewport_paintable.set_accumulated_visual_context_for_fixed_position_descendants(VISUAL_VIEWPORT_NODE_INDEX);

    NearestScrollNodeIndices viewport_nearest_scroll_nodes { viewport_state_for_descendants, viewport_state_for_descendants };
    DescendantVisualContexts viewport_contexts {
        viewport_state_for_descendants,
        viewport_state_for_descendants,
        VISUAL_VIEWPORT_NODE_INDEX,
        viewport_nearest_scroll_nodes,
        viewport_nearest_scroll_nodes,
        viewport_nearest_scroll_nodes,
    };

    VisualContextTreeBuilder builder { viewport_paintable, visual_context_tree, mode };
    for (auto* child = viewport_paintable.first_child_ptr(); child; child = child->next_sibling_ptr())
        builder.build_subtree(*child, viewport_contexts, true);
    builder.build_deferred_anchor_positioned_subtrees();
    return builder.reconcile_outcome();
}

AccumulatedVisualContextTree build_accumulated_visual_context_tree(ViewportPaintable& viewport_paintable)
{
    auto& document = viewport_paintable.document();
    auto visual_context_tree = AccumulatedVisualContextTree::create(visual_viewport_transform_data(document));

    auto viewport_state_for_descendants = visual_context_tree.append(ScrollData { .is_sticky = false }, VISUAL_VIEWPORT_NODE_INDEX);
    viewport_paintable.register_scroll_node(visual_context_tree, viewport_state_for_descendants, viewport_paintable, {});

    stamp_viewport_and_build_subtrees(viewport_paintable, visual_context_tree, viewport_state_for_descendants, VisualContextTreeBuilder::Mode::Build);

    return visual_context_tree;
}

static bool verify_visual_context_tree_reconcile_enabled()
{
    static bool enabled = Core::Environment::has("LADYBIRD_VERIFY_VISUAL_CONTEXT_TREE"sv);
    return enabled;
}

static String visual_context_chain_dump(AccumulatedVisualContextTree const& visual_context_tree, ScrollState const& scroll_state, VisualContextIndex index)
{
    // Anchor shift and scroll compensation payloads embed the referenced scroll node's index,
    // which leaks slot numbering into the dump; identify the referenced node by its owning
    // paintable instead, which the reconciled and the fresh tree share.
    auto scroll_node_owner = [&](VisualContextIndex referenced_index) -> Paintable const* {
        auto slot = visual_context_tree.scroll_state_slot_for_node(referenced_index);
        if (slot == NO_SCROLL_STATE_SLOT)
            return nullptr;
        return scroll_state.state_at_slot(slot).paintable_box_if_alive().ptr();
    };

    StringBuilder builder;
    for (auto i = index;; i = visual_context_tree.node_at(i).parent_index) {
        auto const& node = visual_context_tree.node_at(i);
        if (auto const* shift = node.data.get_pointer<AnchorScrollShift>()) {
            builder.appendff("anchor_scroll_shift(target={}{}{}{})", scroll_node_owner(shift->scroll_node_index),
                shift->negate ? ", negate"sv : ""sv,
                shift->compensate_horizontal_scroll ? ""sv : ", no-x"sv,
                shift->compensate_vertical_scroll ? ""sv : ", no-y"sv);
        } else if (auto const* compensation = node.data.get_pointer<ScrollCompensation>()) {
            builder.appendff("scroll_compensation(target={})", scroll_node_owner(compensation->scroll_node_index));
        } else {
            visual_context_tree.dump(i, builder);
        }
        builder.appendff(" empty_clip={} / ", visual_context_tree.has_empty_effective_clip(i));
        if (i == VISUAL_VIEWPORT_NODE_INDEX)
            break;
    }
    return MUST(builder.to_string());
}

// The scroll-parent linkage deliberately differs from the visual context parent chain, so it is
// compared through its own slot-independent walk.
static String scroll_parent_chain_dump(AccumulatedVisualContextTree const& visual_context_tree, ScrollState const& scroll_state, VisualContextIndex scroll_node_index)
{
    StringBuilder builder;
    for (auto slot = visual_context_tree.scroll_state_slot_for_node(scroll_node_index); slot != NO_SCROLL_STATE_SLOT; slot = scroll_state.state_at_slot(slot).parent_slot()) {
        auto const& state = scroll_state.state_at_slot(slot);
        builder.appendff("{}[{}] / ", state.is_sticky() ? "sticky"sv : "scroll"sv, visual_context_chain_dump(visual_context_tree, scroll_state, state.node_index()));
    }
    return MUST(builder.to_string());
}

// Reruns the fresh build into a scratch tree and compares every connected paintable's chains
// against the reconciled tree. The fresh build overwrites the paintables' stored contexts and the
// scroll state — both inputs the reconcile derives from — so they are snapshotted and restored,
// keeping checker-mode behavior identical to normal mode after each check.
void verify_reconciled_visual_context_tree_matches_fresh_build(ViewportPaintable& viewport_paintable, AccumulatedVisualContextTree const& reconciled_tree)
{
    struct PaintableVisualContexts {
        VisualContextIndex own;
        VisualContextIndex for_descendants;
        VisualContextIndex for_absolute_position_descendants;
        VisualContextIndex for_fixed_position_descendants;
        Optional<VisualContextIndex> fixed_background;
        VisualContextIndex enclosing_scroll_node;
        VisualContextIndex own_scroll_node;
        Vector<VisualContextIndex> owned_nodes;
    };

    auto capture = [](Paintable const& paintable) {
        Vector<VisualContextIndex> owned_nodes { paintable.owned_visual_context_nodes() };
        return PaintableVisualContexts {
            paintable.accumulated_visual_context_index(),
            paintable.accumulated_visual_context_for_descendants_index(),
            paintable.accumulated_visual_context_for_absolute_position_descendants_index(),
            paintable.accumulated_visual_context_for_fixed_position_descendants_index(),
            paintable.fixed_background_visual_context(),
            paintable.enclosing_scroll_node_index(),
            paintable.own_scroll_node_index(),
            move(owned_nodes),
        };
    };

    Vector<Paintable*> paintables;
    HashMap<Paintable*, PaintableVisualContexts> reconciled_contexts;
    viewport_paintable.for_each_in_inclusive_subtree([&](auto& paintable) {
        paintables.append(&paintable);
        reconciled_contexts.set(&paintable, capture(paintable));
        return TraversalDecision::Continue;
    });
    auto reconciled_scroll_state = viewport_paintable.m_scroll_state;

    viewport_paintable.clear_scroll_state();
    auto fresh_tree = build_accumulated_visual_context_tree(viewport_paintable);
    auto const& fresh_scroll_state = viewport_paintable.m_scroll_state;

    for (auto* paintable : paintables) {
        auto const& reconciled = reconciled_contexts.get(paintable).value();
        auto fresh = capture(*paintable);

        struct ChainToCompare {
            StringView name;
            String reconciled_dump;
            String fresh_dump;
        };
        ChainToCompare chains[] = {
            { "own"sv, visual_context_chain_dump(reconciled_tree, reconciled_scroll_state, reconciled.own), visual_context_chain_dump(fresh_tree, fresh_scroll_state, fresh.own) },
            { "descendants"sv, visual_context_chain_dump(reconciled_tree, reconciled_scroll_state, reconciled.for_descendants), visual_context_chain_dump(fresh_tree, fresh_scroll_state, fresh.for_descendants) },
            { "absolute"sv, visual_context_chain_dump(reconciled_tree, reconciled_scroll_state, reconciled.for_absolute_position_descendants), visual_context_chain_dump(fresh_tree, fresh_scroll_state, fresh.for_absolute_position_descendants) },
            { "fixed"sv, visual_context_chain_dump(reconciled_tree, reconciled_scroll_state, reconciled.for_fixed_position_descendants), visual_context_chain_dump(fresh_tree, fresh_scroll_state, fresh.for_fixed_position_descendants) },
            { "fixed-background"sv,
                reconciled.fixed_background.has_value() ? visual_context_chain_dump(reconciled_tree, reconciled_scroll_state, *reconciled.fixed_background) : String {},
                fresh.fixed_background.has_value() ? visual_context_chain_dump(fresh_tree, fresh_scroll_state, *fresh.fixed_background) : String {} },
            { "enclosing-scroll"sv,
                scroll_parent_chain_dump(reconciled_tree, reconciled_scroll_state, reconciled.enclosing_scroll_node),
                scroll_parent_chain_dump(fresh_tree, fresh_scroll_state, fresh.enclosing_scroll_node) },
            { "own-scroll"sv,
                scroll_parent_chain_dump(reconciled_tree, reconciled_scroll_state, reconciled.own_scroll_node),
                scroll_parent_chain_dump(fresh_tree, fresh_scroll_state, fresh.own_scroll_node) },
        };
        for (auto const& chain : chains) {
            if (chain.reconciled_dump == chain.fresh_dump)
                continue;
            dbgln("Reconciled visual context tree mismatch for {} ({} chain)", paintable->layout_node().debug_description(), chain.name);
            dbgln("  reconciled: {}", chain.reconciled_dump);
            dbgln("  fresh:      {}", chain.fresh_dump);
            VERIFY_NOT_REACHED();
        }
    }

    for (auto* paintable : paintables) {
        auto& reconciled = reconciled_contexts.get(paintable).value();
        paintable->set_accumulated_visual_context(reconciled.own);
        paintable->set_accumulated_visual_context_for_descendants(reconciled.for_descendants);
        paintable->set_accumulated_visual_context_for_absolute_position_descendants(reconciled.for_absolute_position_descendants);
        paintable->set_accumulated_visual_context_for_fixed_position_descendants(reconciled.for_fixed_position_descendants);
        if (reconciled.fixed_background.has_value())
            paintable->set_fixed_background_visual_context(*reconciled.fixed_background);
        else
            paintable->clear_fixed_background_visual_context();
        paintable->set_enclosing_scroll_node_index(reconciled.enclosing_scroll_node);
        paintable->set_own_scroll_node_index(reconciled.own_scroll_node);
        paintable->set_owned_visual_context_nodes(reconciled.owned_nodes, reconciled_tree.identity());
    }
    viewport_paintable.m_scroll_state = move(reconciled_scroll_state);
    viewport_paintable.set_needs_to_refresh_scroll_state(true);
}

VisualContextReconcileOutcome reconcile_accumulated_visual_context_tree(ViewportPaintable& viewport_paintable, AccumulatedVisualContextTree& visual_context_tree)
{
    auto& document = viewport_paintable.document();
    visual_context_tree.set_visual_viewport_transform(visual_viewport_transform_data(document));

    // The viewport prelude of the full build always appends its scroll node first, so a tree being
    // reconciled carries it at index 1 with its state slot intact.
    auto viewport_state_for_descendants = VisualContextIndex { 1 };
    VERIFY(visual_context_tree.nodes().size() > viewport_state_for_descendants.value());
    VERIFY(visual_context_tree.node_at(viewport_state_for_descendants).data.has<ScrollData>());

    auto outcome = stamp_viewport_and_build_subtrees(viewport_paintable, visual_context_tree, viewport_state_for_descendants, VisualContextTreeBuilder::Mode::ReconcileInPlace);

    if (verify_visual_context_tree_reconcile_enabled()) [[unlikely]]
        verify_reconciled_visual_context_tree_matches_fresh_build(viewport_paintable, visual_context_tree);

    return outcome;
}

// Patches the transform/effects/perspective values of the box's existing visual context nodes in place.
// Returns false if the box's node structure no longer matches; the caller must then do a full rebuild.
bool update_accumulated_visual_context_values(ViewportPaintable& viewport_paintable, Paintable& paintable_box)
{
    auto& visual_context_tree = viewport_paintable.visual_context_tree();
    // A list recorded against a tree that a fresh build has since replaced indexes nothing
    // meaningful in the current one.
    if (paintable_box.owned_visual_context_nodes_tree_identity() != visual_context_tree.identity())
        return false;
    auto owned_node_indices = paintable_box.owned_visual_context_nodes();

    auto pixel_ratio = viewport_paintable.document().page().client().device_pixels_per_css_pixel();
    auto const& computed_values = paintable_box.computed_values();

    auto effects = compute_effects_data(paintable_box, computed_values, pixel_ratio);
    auto transform = compute_transform(paintable_box, computed_values, pixel_ratio);
    auto perspective = compute_perspective_data(paintable_box, computed_values, static_cast<float>(pixel_ratio));

    paintable_box.set_has_non_invertible_css_transform(transform.has_value() && !transform->matrix.is_invertible());

    bool found_transform = false;
    bool found_effects = false;
    bool found_perspective = false;
    for (auto node_index : owned_node_indices) {
        auto& node = visual_context_tree.node_at(node_index);
        if (auto* transform_data = node.data.get_pointer<TransformData>()) {
            if (!transform.has_value())
                return false;
            *transform_data = *transform;
            found_transform = true;
        } else if (auto* effects_data = node.data.get_pointer<EffectsData>()) {
            if (!effects.has_value())
                return false;
            *effects_data = *effects;
            found_effects = true;
        } else if (auto* perspective_data = node.data.get_pointer<PerspectiveData>()) {
            if (!perspective.has_value())
                return false;
            *perspective_data = *perspective;
            found_perspective = true;
        }
    }

    if (transform.has_value() != found_transform)
        return false;
    if (effects.has_value() != found_effects)
        return false;
    if (perspective.has_value() != found_perspective)
        return false;
    return true;
}

void update_visual_viewport_accumulated_visual_context(ViewportPaintable& viewport_paintable)
{
    VERIFY(viewport_paintable.m_visual_context_tree.has_value());
    viewport_paintable.m_visual_context_tree->set_visual_viewport_transform(visual_viewport_transform_data(viewport_paintable.document()));
}

}

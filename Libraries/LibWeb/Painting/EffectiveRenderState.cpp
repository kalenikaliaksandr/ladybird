/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGfx/Matrix4x4.h>
#include <LibWeb/Painting/EffectiveRenderState.h>

namespace Web::Painting {

Optional<CSSPixelPoint> EffectiveRenderState::transform_point_for_hit_test(
    CSSPixelPoint screen_point,
    ScrollStateSnapshot const& scroll_state) const
{
    // Build the chain from root to this node.
    // We need to walk root-to-leaf because transformations are cumulative:
    // the root's transform is applied first in painting, so its inverse
    // must be applied first in hit-testing.
    Vector<EffectiveRenderState const*> chain;
    for (auto const* node = this; node; node = node->parent().ptr())
        chain.append(node);

    // Walk from root to leaf, applying inverse transformations and checking clips.
    // This reverses what the display list player does during painting.
    //
    // We track the transformation from current space back to screen space so we can
    // check clip containment correctly. Clips are stored in layout coordinates with
    // a scroll adjustment that gives screen coordinates. When checking a clip, we
    // transform the current point back to screen space for comparison.
    auto point = screen_point;

    // Track transformation from current point space to screen space
    Gfx::AffineTransform current_to_screen;

    // Track cumulative scroll offset we've undone (to transform point back to screen space)
    CSSPixelPoint cumulative_scroll_undone;

    for (size_t i = chain.size(); i > 0; --i) {
        auto const* node = chain[i - 1];

        // 1. Inverse perspective (if present).
        // Perspective is applied first in painting, so its inverse is applied first here.
        if (node->perspective().has_value()) {
            auto affine = Gfx::extract_2d_affine_transform(node->perspective().value());
            auto inverse = affine.inverse();
            if (!inverse.has_value())
                return {}; // Singular matrix, point is undefined
            point = inverse->map(point.to_type<float>()).to_type<CSSPixels>();
            current_to_screen = affine.multiply(current_to_screen);
        }

        // 2. Undo scroll offset.
        // In painting, we translate by own_offset (which is -scroll_position).
        // To reverse this, we subtract own_offset (which adds scroll_position).
        if (auto scroll_id = node->scroll_frame_id(); scroll_id.has_value()) {
            auto offset = scroll_state.own_offset_for_frame_with_id(*scroll_id);
            point.set_x(point.x() - offset.x());
            point.set_y(point.y() - offset.y());
            cumulative_scroll_undone.translate_by(offset);
        }

        // 3. Inverse transform.
        // Transform is applied around the transform origin.
        if (auto tf = node->transform_frame()) {
            auto origin = tf->origin();
            auto affine = Gfx::extract_2d_affine_transform(tf->matrix());
            auto inverse = affine.inverse();
            if (!inverse.has_value())
                return {}; // Singular matrix, point is undefined

            // Transform around origin: translate to origin, apply inverse, translate back
            auto offset_point = point - origin;
            auto transformed = inverse->map(offset_point.to_type<float>()).to_type<CSSPixels>();
            point = transformed + origin;

            // Update current_to_screen to include this transform (around origin)
            auto origin_f = origin.to_type<float>();
            auto transform_around_origin = Gfx::AffineTransform {}
                                               .translate(origin_f)
                                               .multiply(affine)
                                               .translate(-origin_f);
            current_to_screen = transform_around_origin.multiply(current_to_screen);
        }

        // 4. Check clip containment.
        // Transform the current point back to screen space and check against the
        // clip rect (which is stored in layout coordinates + scroll adjustment).
        if (auto cf = node->clip_frame()) {
            auto clip_rect = cf->clip_rect().rect;
            if (auto scroll_frame_id = cf->clip_rect().enclosing_scroll_frame_id; scroll_frame_id.has_value()) {
                clip_rect.translate_by(scroll_state.cumulative_offset_for_frame_with_id(*scroll_frame_id));
            }

            // Transform point back to screen space for clip check
            auto point_in_screen = current_to_screen.map(point.to_type<float>()).to_type<CSSPixels>();
            point_in_screen.translate_by(cumulative_scroll_undone);

            if (!clip_rect.contains(point_in_screen))
                return {}; // Point is outside clip region
        }
    }

    return point;
}

}

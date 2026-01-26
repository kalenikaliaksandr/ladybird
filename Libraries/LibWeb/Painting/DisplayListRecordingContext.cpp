/*
 * Copyright (c) 2018-2022, Andreas Kling <andreas@ladybird.org>
 * Copyright (c) 2022, Sam Atkins <atkinssj@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/CSS/ComputedValues.h>
#include <LibWeb/Painting/DisplayListRecorder.h>
#include <LibWeb/Painting/DisplayListRecordingContext.h>
#include <LibWeb/Painting/PaintableBox.h>
#include <LibWeb/Painting/ScrollHitTestScene.h>

namespace Web {

static u64 s_next_paint_generation_id = 0;

DisplayListRecordingContext::DisplayListRecordingContext(Painting::DisplayListRecorder& display_list_recorder, Palette const& palette, double device_pixels_per_css_pixel, ChromeMetrics const& chrome_metrics)
    : m_display_list_recorder(display_list_recorder)
    , m_palette(palette)
    , m_device_pixel_converter(device_pixels_per_css_pixel)
    , m_chrome_metrics(chrome_metrics)
    , m_paint_generation_id(s_next_paint_generation_id++)
{
}

CSSPixelRect DisplayListRecordingContext::css_viewport_rect() const
{
    return {
        m_device_viewport_rect.x().value() / m_device_pixel_converter.device_pixels_per_css_pixel(),
        m_device_viewport_rect.y().value() / m_device_pixel_converter.device_pixels_per_css_pixel(),
        m_device_viewport_rect.width().value() / m_device_pixel_converter.device_pixels_per_css_pixel(),
        m_device_viewport_rect.height().value() / m_device_pixel_converter.device_pixels_per_css_pixel()
    };
}

DevicePixels DisplayListRecordingContext::rounded_device_pixels(CSSPixels css_pixels) const
{
    return m_device_pixel_converter.rounded_device_pixels(css_pixels);
}

DevicePixels DisplayListRecordingContext::enclosing_device_pixels(CSSPixels css_pixels) const
{
    return m_device_pixel_converter.enclosing_device_pixels(css_pixels);
}

DevicePixels DisplayListRecordingContext::floored_device_pixels(CSSPixels css_pixels) const
{
    return m_device_pixel_converter.floored_device_pixels(css_pixels);
}

DevicePixelPoint DisplayListRecordingContext::rounded_device_point(CSSPixelPoint point) const
{
    return m_device_pixel_converter.rounded_device_point(point);
}

DevicePixelPoint DisplayListRecordingContext::floored_device_point(CSSPixelPoint point) const
{
    return m_device_pixel_converter.floored_device_point(point);
}

DevicePixelRect DisplayListRecordingContext::enclosing_device_rect(CSSPixelRect rect) const
{
    return m_device_pixel_converter.enclosing_device_rect(rect);
}

DevicePixelRect DisplayListRecordingContext::rounded_device_rect(CSSPixelRect rect) const
{
    return m_device_pixel_converter.rounded_device_rect(rect);
}

DevicePixelSize DisplayListRecordingContext::enclosing_device_size(CSSPixelSize size) const
{
    return m_device_pixel_converter.enclosing_device_size(size);
}

DevicePixelSize DisplayListRecordingContext::rounded_device_size(CSSPixelSize size) const
{
    return m_device_pixel_converter.rounded_device_size(size);
}

CSSPixels DisplayListRecordingContext::scale_to_css_pixels(DevicePixels device_pixels) const
{
    return CSSPixels::nearest_value_for(device_pixels.value() / m_device_pixel_converter.device_pixels_per_css_pixel());
}

CSSPixelPoint DisplayListRecordingContext::scale_to_css_point(DevicePixelPoint point) const
{
    return {
        scale_to_css_pixels(point.x()),
        scale_to_css_pixels(point.y())
    };
}

CSSPixelSize DisplayListRecordingContext::scale_to_css_size(DevicePixelSize size) const
{
    return {
        scale_to_css_pixels(size.width()),
        scale_to_css_pixels(size.height())
    };
}

CSSPixelRect DisplayListRecordingContext::scale_to_css_rect(DevicePixelRect rect) const
{
    return {
        scale_to_css_point(rect.location()),
        scale_to_css_size(rect.size())
    };
}

void DisplayListRecordingContext::begin_scroll_hit_test_scene()
{
    m_scroll_hit_test_scene = Painting::ScrollHitTestScene::create();
    m_scroll_hit_test_stacking_order = 0;
}

void DisplayListRecordingContext::emit_scroll_hit_test_item(Painting::PaintableBox const& paintable)
{
    if (!m_scroll_hit_test_scene)
        return;

    // Determine opaqueness
    auto opaqueness = Painting::HitTestOpaqueness::Opaque;
    auto const& computed_values = paintable.computed_values();
    if (computed_values.opacity() == 0.0f)
        opaqueness = Painting::HitTestOpaqueness::Transparent;
    if (computed_values.pointer_events() == CSS::PointerEvents::None)
        opaqueness = Painting::HitTestOpaqueness::Transparent;

    // Determine if this element is scrollable
    Optional<size_t> scroll_frame_id;
    if (paintable.has_scrollable_overflow()) {
        if (auto scroll_frame = paintable.own_scroll_frame()) {
            if (!scroll_frame->is_sticky())
                scroll_frame_id = scroll_frame->id();
        }
    }

    m_scroll_hit_test_scene->add_item({
        .hit_rect = paintable.absolute_border_box_rect(),
        .visual_context = paintable.accumulated_visual_context(),
        .stacking_order = m_scroll_hit_test_stacking_order++,
        .scroll_frame_id = scroll_frame_id,
        .opaqueness = opaqueness,
    });
}

void DisplayListRecordingContext::finalize_scroll_hit_test_scene()
{
    VERIFY(m_scroll_hit_test_scene);
    m_scroll_hit_test_scene->finalize();
    m_display_list_recorder.display_list().set_scroll_hit_test_scene(m_scroll_hit_test_scene.release_nonnull());
}

}

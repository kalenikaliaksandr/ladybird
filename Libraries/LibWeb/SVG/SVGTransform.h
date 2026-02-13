/*
 * Copyright (c) 2024, MacDue <macdue@dueutil.tech>
 * Copyright (c) 2024, Jamie Mansfield <jmansfield@cadixdev.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/Bindings/PlatformObject.h>
#include <LibWeb/Geometry/DOMMatrix.h>

namespace Web::SVG {

// https://svgwg.org/svg2-draft/single-page.html#coords-InterfaceSVGTransform
class SVGTransform final : public Bindings::PlatformObject {
    WEB_PLATFORM_OBJECT(SVGTransform, Bindings::PlatformObject);
    GC_DECLARE_ALLOCATOR(SVGTransform);

public:
    enum class Type : u16 {
        Unknown = 0,
        Matrix = 1,
        Translate = 2,
        Scale = 3,
        Rotate = 4,
        SkewX = 5,
        SkewY = 6,
    };

    [[nodiscard]] static GC::Ref<SVGTransform> create(JS::Realm& realm);
    [[nodiscard]] static GC::Ref<SVGTransform> create(JS::Realm& realm, Type type, float angle, float p1, float p2);
    [[nodiscard]] static GC::Ref<SVGTransform> create_matrix(JS::Realm& realm, float a, float b, float c, float d, float e, float f);
    virtual ~SVGTransform() override;

    Type type();
    float angle();
    GC::Ref<Geometry::DOMMatrix> matrix();

    WebIDL::ExceptionOr<void> set_matrix(Geometry::DOMMatrix2DInit& matrix);
    void set_translate(float tx, float ty);
    void set_scale(float sx, float sy);
    void set_rotate(float angle, float cx, float cy);
    void set_skew_x(float angle);
    void set_skew_y(float angle);

private:
    SVGTransform(JS::Realm& realm);
    SVGTransform(JS::Realm& realm, Type type, float angle, float p1, float p2);

    virtual void initialize(JS::Realm& realm) override;
    virtual void visit_edges(Cell::Visitor& visitor) override;

    void update_matrix();

    Type m_type { Type::Unknown };
    float m_angle { 0 };
    float m_param1 { 0 };
    float m_param2 { 0 };
    // For matrix() type, store all 6 values
    float m_matrix_a { 1 };
    float m_matrix_b { 0 };
    float m_matrix_c { 0 };
    float m_matrix_d { 1 };
    float m_matrix_e { 0 };
    float m_matrix_f { 0 };

    GC::Ptr<Geometry::DOMMatrix> m_cached_matrix;
};

}

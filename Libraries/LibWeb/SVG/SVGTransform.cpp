/*
 * Copyright (c) 2024, MacDue <macdue@dueutil.tech>
 * Copyright (c) 2024, Jamie Mansfield <jmansfield@cadixdev.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Math.h>
#include <LibWeb/Bindings/Intrinsics.h>
#include <LibWeb/Bindings/SVGTransformPrototype.h>
#include <LibWeb/SVG/SVGTransform.h>
#include <LibWeb/WebIDL/ExceptionOr.h>

namespace Web::SVG {

GC_DEFINE_ALLOCATOR(SVGTransform);

GC::Ref<SVGTransform> SVGTransform::create(JS::Realm& realm)
{
    return realm.create<SVGTransform>(realm);
}

GC::Ref<SVGTransform> SVGTransform::create(JS::Realm& realm, Type type, float angle, float p1, float p2)
{
    return realm.create<SVGTransform>(realm, type, angle, p1, p2);
}

GC::Ref<SVGTransform> SVGTransform::create_matrix(JS::Realm& realm, float a, float b, float c, float d, float e, float f)
{
    auto transform = realm.create<SVGTransform>(realm);
    transform->m_type = Type::Matrix;
    transform->m_matrix_a = a;
    transform->m_matrix_b = b;
    transform->m_matrix_c = c;
    transform->m_matrix_d = d;
    transform->m_matrix_e = e;
    transform->m_matrix_f = f;
    return transform;
}

SVGTransform::SVGTransform(JS::Realm& realm)
    : PlatformObject(realm)
{
}

SVGTransform::SVGTransform(JS::Realm& realm, Type type, float angle, float p1, float p2)
    : PlatformObject(realm)
    , m_type(type)
    , m_angle(angle)
    , m_param1(p1)
    , m_param2(p2)
{
}

SVGTransform::~SVGTransform() = default;

void SVGTransform::initialize(JS::Realm& realm)
{
    WEB_SET_PROTOTYPE_FOR_INTERFACE(SVGTransform);
    Base::initialize(realm);
}

void SVGTransform::visit_edges(Cell::Visitor& visitor)
{
    Base::visit_edges(visitor);
    visitor.visit(m_cached_matrix);
}

void SVGTransform::update_matrix()
{
    if (!m_cached_matrix)
        return;

    auto& m = *m_cached_matrix;
    switch (m_type) {
    case Type::Translate:
        m.set_a(1);
        m.set_b(0);
        m.set_c(0);
        m.set_d(1);
        m.set_e(m_param1);
        m.set_f(m_param2);
        break;
    case Type::Scale:
        m.set_a(m_param1);
        m.set_b(0);
        m.set_c(0);
        m.set_d(m_param2);
        m.set_e(0);
        m.set_f(0);
        break;
    case Type::Rotate: {
        auto angle_radians = AK::to_radians(static_cast<double>(m_angle));
        auto cos_a = cos(angle_radians);
        auto sin_a = sin(angle_radians);
        auto cx = static_cast<double>(m_param1);
        auto cy = static_cast<double>(m_param2);
        // rotate(a, cx, cy) = translate(cx, cy) * rotate(a) * translate(-cx, -cy)
        m.set_a(cos_a);
        m.set_b(sin_a);
        m.set_c(-sin_a);
        m.set_d(cos_a);
        m.set_e(cx - cos_a * cx + sin_a * cy);
        m.set_f(cy - sin_a * cx - cos_a * cy);
        break;
    }
    case Type::SkewX: {
        auto angle_radians = AK::to_radians(static_cast<double>(m_angle));
        m.set_a(1);
        m.set_b(0);
        m.set_c(tan(angle_radians));
        m.set_d(1);
        m.set_e(0);
        m.set_f(0);
        break;
    }
    case Type::SkewY: {
        auto angle_radians = AK::to_radians(static_cast<double>(m_angle));
        m.set_a(1);
        m.set_b(tan(angle_radians));
        m.set_c(0);
        m.set_d(1);
        m.set_e(0);
        m.set_f(0);
        break;
    }
    case Type::Matrix:
        m.set_a(m_matrix_a);
        m.set_b(m_matrix_b);
        m.set_c(m_matrix_c);
        m.set_d(m_matrix_d);
        m.set_e(m_matrix_e);
        m.set_f(m_matrix_f);
        break;
    case Type::Unknown:
        break;
    }
}

// https://svgwg.org/svg2-draft/single-page.html#coords-__svg__SVGTransform__type
SVGTransform::Type SVGTransform::type()
{
    return m_type;
}

// https://svgwg.org/svg2-draft/single-page.html#coords-__svg__SVGTransform__angle
float SVGTransform::angle()
{
    return m_angle;
}

// https://svgwg.org/svg2-draft/single-page.html#coords-__svg__SVGTransform__matrix
GC::Ref<Geometry::DOMMatrix> SVGTransform::matrix()
{
    if (!m_cached_matrix) {
        m_cached_matrix = Geometry::DOMMatrix::create(realm());
        update_matrix();
    }
    return *m_cached_matrix;
}

// https://svgwg.org/svg2-draft/single-page.html#coords-__svg__SVGTransform__setMatrix
WebIDL::ExceptionOr<void> SVGTransform::set_matrix(Geometry::DOMMatrix2DInit& init)
{
    auto result = TRY(Geometry::DOMMatrix::create_from_dom_matrix_2d_init(realm(), init));
    m_type = Type::Matrix;
    m_angle = 0;
    m_matrix_a = result->m11();
    m_matrix_b = result->m12();
    m_matrix_c = result->m21();
    m_matrix_d = result->m22();
    m_matrix_e = result->m41();
    m_matrix_f = result->m42();
    update_matrix();
    return {};
}

// https://svgwg.org/svg2-draft/single-page.html#coords-__svg__SVGTransform__setTranslate
void SVGTransform::set_translate(float tx, float ty)
{
    m_type = Type::Translate;
    m_angle = 0;
    m_param1 = tx;
    m_param2 = ty;
    update_matrix();
}

// https://svgwg.org/svg2-draft/single-page.html#coords-__svg__SVGTransform__setScale
void SVGTransform::set_scale(float sx, float sy)
{
    m_type = Type::Scale;
    m_angle = 0;
    m_param1 = sx;
    m_param2 = sy;
    update_matrix();
}

// https://svgwg.org/svg2-draft/single-page.html#coords-__svg__SVGTransform__setRotate
void SVGTransform::set_rotate(float angle, float cx, float cy)
{
    m_type = Type::Rotate;
    m_angle = angle;
    m_param1 = cx;
    m_param2 = cy;
    update_matrix();
}

// https://svgwg.org/svg2-draft/single-page.html#coords-__svg__SVGTransform__setSkewX
void SVGTransform::set_skew_x(float angle)
{
    m_type = Type::SkewX;
    m_angle = angle;
    m_param1 = 0;
    m_param2 = 0;
    update_matrix();
}

// https://svgwg.org/svg2-draft/single-page.html#coords-__svg__SVGTransform__setSkewY
void SVGTransform::set_skew_y(float angle)
{
    m_type = Type::SkewY;
    m_angle = angle;
    m_param1 = 0;
    m_param2 = 0;
    update_matrix();
}

}

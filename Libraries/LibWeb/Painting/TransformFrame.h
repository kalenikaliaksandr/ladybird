/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/AtomicRefCounted.h>
#include <LibGfx/Matrix4x4.h>
#include <LibWeb/PixelUnits.h>

namespace Web::Painting {

class TransformFrame : public AtomicRefCounted<TransformFrame> {
public:
    static NonnullRefPtr<TransformFrame> create(Gfx::FloatMatrix4x4 const& matrix, CSSPixelPoint origin)
    {
        return adopt_ref(*new TransformFrame(matrix, origin));
    }

    Gfx::FloatMatrix4x4 const& matrix() const { return m_matrix; }
    CSSPixelPoint origin() const { return m_origin; }

    void set_matrix(Gfx::FloatMatrix4x4 const& matrix) { m_matrix = matrix; }
    void set_origin(CSSPixelPoint origin) { m_origin = origin; }

private:
    TransformFrame(Gfx::FloatMatrix4x4 const& matrix, CSSPixelPoint origin)
        : m_matrix(matrix)
        , m_origin(origin)
    {
    }

    Gfx::FloatMatrix4x4 m_matrix;
    CSSPixelPoint m_origin;
};

}

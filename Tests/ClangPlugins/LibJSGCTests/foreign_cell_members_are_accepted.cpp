/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// RUN: %clang++ -Xclang -verify %plugin_opts% -c %s -o %t 2>&1
// expected-no-diagnostics

#include <AK/Optional.h>
#include <AK/Vector.h>
#include <LibGC/Cell.h>
#include <LibGC/Root.h>
#include <LibGC/RootVector.h>

// Types naming foreign cells take no GC_CELL macro, since they have no vtable for its class_name() to override.
class ForeignObject : public GC::ForeignCell { };

class ForeignArray : public ForeignObject { };

class CellHoldingForeignCells : public GC::Cell {
    GC_CELL(CellHoldingForeignCells, GC::Cell);

    virtual void visit_edges(Visitor& visitor) override
    {
        Base::visit_edges(visitor);
        visitor.visit(m_object);
        visitor.visit(m_array);
        visitor.visit(m_objects);
        if (m_optional_object.has_value())
            visitor.visit(*m_optional_object);
    }

    GC::Ptr<ForeignObject> m_object;
    GC::Ref<ForeignArray> m_array;
    Vector<GC::Ref<ForeignObject>> m_objects;
    Optional<GC::Ptr<ForeignObject>> m_optional_object;
    GC::RawPtr<ForeignObject> m_object_kept_alive_elsewhere;
};

struct NonCellHoldingForeignCells {
    GC::Ptr<ForeignObject> m_object;
    GC::Root<ForeignObject> m_rooted_object;
    GC::RootVector<GC::Ref<ForeignObject>> m_rooted_objects;
};

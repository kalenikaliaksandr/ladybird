/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// RUN: %clang++ -Xclang -verify %plugin_opts% -c %s -o %t 2>&1

#include <AK/Optional.h>
#include <AK/Vector.h>
#include <LibGC/Cell.h>

class ForeignObject : public GC::ForeignCell { };

class ForeignArray : public ForeignObject { };

class CellNotVisitingForeignCells : public GC::Cell {
    GC_CELL(CellNotVisitingForeignCells, GC::Cell);

    virtual void visit_edges(Visitor& visitor) override
    {
        Base::visit_edges(visitor);
    }

    // expected-error@+1 {{GC-allocated member is not visited in CellNotVisitingForeignCells::visit_edges}}
    GC::Ptr<ForeignObject> m_object;

    // expected-error@+1 {{GC-allocated member is not visited in CellNotVisitingForeignCells::visit_edges}}
    GC::Ref<ForeignArray> m_array;

    // expected-error@+1 {{GC-allocated member is not visited in CellNotVisitingForeignCells::visit_edges}}
    Vector<GC::Ref<ForeignObject>> m_objects;

    // expected-error@+1 {{GC-allocated member is not visited in CellNotVisitingForeignCells::visit_edges}}
    Optional<GC::Ptr<ForeignObject>> m_optional_object;
};

// expected-error@+1 {{GC::Cell-inheriting class CellWithoutVisitEdges contains a GC-allocated member 'm_object' but has no visit_edges method}}
class CellWithoutVisitEdges : public GC::Cell {
    GC_CELL(CellWithoutVisitEdges, GC::Cell);

    GC::Ptr<ForeignObject> m_object;
};

struct SubstructHoldingForeignCell {
    GC::Ptr<ForeignObject> m_object;
};

class CellNotVisitingSubstruct : public GC::Cell {
    GC_CELL(CellNotVisitingSubstruct, GC::Cell);

    virtual void visit_edges(Visitor& visitor) override
    {
        Base::visit_edges(visitor);
    }

    // expected-error@+1 {{Member m_substruct contains GC pointers but its type has no visit_edges method}}
    SubstructHoldingForeignCell m_substruct;
};

class NonCellHoldingForeignCellsUnwrapped {
public:
    explicit NonCellHoldingForeignCellsUnwrapped(ForeignObject& object)
        : m_object_reference(object)
    {
    }

private:
    // expected-error@+1 {{reference to GC::Cell type should be wrapped in GC::Ref}}
    ForeignObject& m_object_reference;
    // expected-error@+1 {{pointer to GC::Cell type should be wrapped in GC::Ptr}}
    ForeignArray* m_array_pointer;
    // expected-error@+1 {{pointer to GC::Cell type should be wrapped in GC::Ptr}}
    Vector<ForeignObject*> m_object_pointers;
};

struct NonCellStoringForeignCellsByValue {
    // expected-error@+1 {{GC::ForeignCell type ForeignObject is allocated by its foreign implementation, not stored by value}}
    ForeignObject m_object;
    // expected-error@+1 {{GC::ForeignCell type ForeignArray is allocated by its foreign implementation, not stored by value}}
    ForeignArray m_arrays[2];
};

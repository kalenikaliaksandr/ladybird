/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/HashTable.h>
#include <LibGC/Cell.h>
#include <LibGC/CellAllocator.h>
#include <LibGC/Heap.h>
#include <LibGC/Ptr.h>
#include <LibGC/Weak.h>
#include <LibGC/WeakInlines.h>

#include "EmbeddingTest.h"

// One of the embedder's GC cells, like the settings object LibWeb keeps in a realm or the incumbent settings it keeps
// in a job callback. It can hold a cell of the runtime in turn, which it visits like any other cell.
class HostDefinedCell final : public GC::Cell {
    GC_CELL(HostDefinedCell, GC::Cell);
    GC_DECLARE_ALLOCATOR(HostDefinedCell);

public:
    void* held_runtime_cell() const { return m_held_runtime_cell.ptr(); }
    void hold_runtime_cell(void* cell) { m_held_runtime_cell = static_cast<GC::Cell*>(cell); }

private:
    virtual void visit_edges(Visitor& visitor) override
    {
        Base::visit_edges(visitor);
        visitor.visit(m_held_runtime_cell);
    }

    GC::Ptr<GC::Cell> m_held_runtime_cell;
};

GC_DEFINE_ALLOCATOR(HostDefinedCell);

static void* address_of(GC::Weak<HostDefinedCell> const& cell)
{
    return cell.ptr().ptr();
}

// Collects garbage once the stack below the caller no longer holds pointers left over from earlier calls, which the
// conservative scan would treat as roots.
static NEVER_INLINE void collect_garbage()
{
    u8 volatile filler[8 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
    GC::Heap::the().collect_garbage();
}

struct GlobalThisValueCreation {
    JSVM* vm { nullptr };
    size_t call_count { 0 };
    JSRealm* realm { nullptr };
    bool realm_had_global_object { false };
    GC::Weak<HostDefinedCell> host_defined_cell;
};

// Picks the realm's %Object.prototype% as the this value of its global environment, after giving the realm a
// host-defined cell and collecting garbage while the realm is still being created.
static JSObject* create_global_this_value(void* context, JSRealm* realm)
{
    auto& creation = *static_cast<GlobalThisValueCreation*>(context);
    ++creation.call_count;
    creation.realm = realm;
    creation.realm_had_global_object = js_realm_global_object(realm) != nullptr;

    auto host_defined_cell = GC::Heap::the().allocate<HostDefinedCell>();
    creation.host_defined_cell = host_defined_cell;
    js_realm_set_host_defined(realm, host_defined_cell.ptr());
    collect_garbage();

    return js_realm_intrinsic(creation.vm, realm, JS_INTRINSIC_OBJECT_PROTOTYPE);
}

TEST_CASE(a_new_realm_runs_in_the_execution_context_the_embedder_provides)
{
    auto embedded_vm = EmbeddedVM::create(EmbeddedVM::process_default_heap_options);
    GlobalThisValueCreation creation;
    creation.vm = embedded_vm->vm();
    auto completion = embedded_vm->initialize_realm_with_global_this_value(create_global_this_value, &creation);
    EXPECT_EQ(completion.variant, JS_COMPLETION_NORMAL);

    auto* realm = embedded_vm->realm();
    EXPECT(realm != nullptr);
    EXPECT_EQ(creation.call_count, 1u);
    EXPECT_EQ(creation.realm, realm);
    EXPECT(!creation.realm_had_global_object);
    EXPECT(js_realm_global_object(realm) != nullptr);
    EXPECT_NE(js_realm_global_object(realm), embedded_vm->intrinsic(JS_INTRINSIC_OBJECT_PROTOTYPE));
    EXPECT(js_realm_global_environment(realm) != nullptr);

    // The cell the callback gave the realm survived the collection it ran.
    EXPECT(creation.host_defined_cell);
    EXPECT_EQ(js_realm_host_defined(realm), address_of(creation.host_defined_cell));
}

TEST_CASE(each_intrinsic_exists_once_and_belongs_to_its_realm)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);

    HashTable<JSObject*> intrinsics;
    for (JSIntrinsic intrinsic = 0; intrinsic < JS_INTRINSIC_COUNT; ++intrinsic) {
        auto* object = embedded_vm->intrinsic(intrinsic);
        EXPECT(object != nullptr);
        EXPECT_EQ(embedded_vm->intrinsic(intrinsic), object);
        intrinsics.set(object);
    }
    EXPECT_EQ(intrinsics.size(), static_cast<size_t>(JS_INTRINSIC_COUNT));
    EXPECT(embedded_vm->intrinsic(JS_INTRINSIC_COUNT) == nullptr);

    auto completion = js_realm_get_function_realm(embedded_vm->vm(), embedded_vm->intrinsic(JS_INTRINSIC_JSON_STRINGIFY_FUNCTION));
    EXPECT_EQ(pointer_of_payload<JSRealm>(completion), embedded_vm->realm());
}

struct HostDefinedCells {
    GC::Weak<HostDefinedCell> settings;
    GC::Weak<HostDefinedCell> incumbent_settings;
};

// Gives the realm a settings cell that holds a job callback, whose custom data is an incumbent settings cell. Nothing
// but the realm holds any of them once this returns.
static NEVER_INLINE HostDefinedCells give_the_realm_host_defined_cells(EmbeddedVM& embedded_vm)
{
    auto settings = GC::Heap::the().allocate<HostDefinedCell>();
    auto incumbent_settings = GC::Heap::the().allocate<HostDefinedCell>();
    auto* job_callback = js_realm_job_callback_create(embedded_vm.vm(), embedded_vm.intrinsic(JS_INTRINSIC_JSON_STRINGIFY_FUNCTION), incumbent_settings.ptr());
    settings->hold_runtime_cell(job_callback);
    js_realm_set_host_defined(embedded_vm.realm(), settings.ptr());
    return { settings, incumbent_settings };
}

static NEVER_INLINE bool realm_holds_its_host_defined_cells(EmbeddedVM& embedded_vm, HostDefinedCells const& cells)
{
    auto* realm = embedded_vm.realm();
    auto* host_defined = js_realm_host_defined(realm);
    auto* host_defined_read_inline = *reinterpret_cast<void**>(reinterpret_cast<u8*>(realm) + JS_LAYOUT_REALM_HOST_DEFINED_OFFSET);
    if (!cells.settings || host_defined != address_of(cells.settings) || host_defined_read_inline != host_defined)
        return false;
    auto* job_callback = static_cast<JSJobCallback*>(cells.settings->held_runtime_cell());
    return cells.incumbent_settings
        && js_realm_job_callback_custom_data(job_callback) == address_of(cells.incumbent_settings)
        && js_realm_job_callback_callback(job_callback) == embedded_vm.intrinsic(JS_INTRINSIC_JSON_STRINGIFY_FUNCTION);
}

TEST_CASE(host_defined_cells_live_as_long_as_the_realm_holds_them)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto cells = give_the_realm_host_defined_cells(*embedded_vm);

    collect_garbage();
    EXPECT(realm_holds_its_host_defined_cells(*embedded_vm, cells));

    js_realm_set_host_defined(embedded_vm->realm(), nullptr);
    collect_garbage();
    EXPECT(!cells.settings);
    EXPECT(!cells.incumbent_settings);
}

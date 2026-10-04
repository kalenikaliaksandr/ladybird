/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/BitCast.h>
#include <AK/Vector.h>
#include <LibJS/Embedding/ABI.h>
#include <LibJS/Embedding/Layout.h>

#include "EmbeddingTest.h"

static_assert(sizeof(JSScriptOrModule) == JS_LAYOUT_SCRIPT_OR_MODULE_SIZE);
static_assert(offsetof(JSScriptOrModule, tag) == JS_LAYOUT_SCRIPT_OR_MODULE_TAG_OFFSET);
static_assert(offsetof(JSScriptOrModule, cell) == JS_LAYOUT_SCRIPT_OR_MODULE_PAYLOAD_OFFSET);

namespace {

template<typename Field, size_t offset, size_t size_in_layout>
Field& field_of(JSExecutionContext* execution_context)
{
    static_assert(sizeof(Field) == size_in_layout);
    return *reinterpret_cast<Field*>(reinterpret_cast<u8*>(execution_context) + offset);
}

u32& slot_count_of(JSExecutionContext* execution_context)
{
    return field_of<u32, JS_LAYOUT_EXECUTION_CONTEXT_REGISTERS_AND_CONSTANTS_AND_LOCALS_AND_ARGUMENTS_COUNT_OFFSET, JS_LAYOUT_EXECUTION_CONTEXT_REGISTERS_AND_CONSTANTS_AND_LOCALS_AND_ARGUMENTS_COUNT_SIZE>(execution_context);
}

u32& argument_count_of(JSExecutionContext* execution_context)
{
    return field_of<u32, JS_LAYOUT_EXECUTION_CONTEXT_ARGUMENT_COUNT_OFFSET, JS_LAYOUT_EXECUTION_CONTEXT_ARGUMENT_COUNT_SIZE>(execution_context);
}

u32& program_counter_of(JSExecutionContext* execution_context)
{
    return field_of<u32, JS_LAYOUT_EXECUTION_CONTEXT_PROGRAM_COUNTER_OFFSET, JS_LAYOUT_EXECUTION_CONTEXT_PROGRAM_COUNTER_SIZE>(execution_context);
}

GCCell*& realm_of(JSExecutionContext* execution_context)
{
    return field_of<GCCell*, JS_LAYOUT_EXECUTION_CONTEXT_REALM_OFFSET, JS_LAYOUT_EXECUTION_CONTEXT_REALM_SIZE>(execution_context);
}

JSScriptOrModule& script_or_module_of(JSExecutionContext* execution_context)
{
    return field_of<JSScriptOrModule, JS_LAYOUT_EXECUTION_CONTEXT_SCRIPT_OR_MODULE_OFFSET, JS_LAYOUT_EXECUTION_CONTEXT_SCRIPT_OR_MODULE_SIZE>(execution_context);
}

u64* arguments_of(JSExecutionContext* execution_context)
{
    auto* slots = reinterpret_cast<u64*>(reinterpret_cast<u8*>(execution_context) + JS_LAYOUT_EXECUTION_CONTEXT_SIZE);
    return slots + slot_count_of(execution_context) - argument_count_of(execution_context);
}

// A cell of the embedder: the word it owns, the header bytes LibGC reads and writes, an id that tells the test which
// cell was finalized, and room for the free list link that LibGC stores in a cell once it is dead.
struct EmbedderCell {
    void const* class_word;
    bool mark;
    u8 state;
    u8 kind;
    u32 id;
    u8 room_for_the_free_list_link[16];
};

static_assert(offsetof(EmbedderCell, mark) == JS_LAYOUT_CELL_MARK_OFFSET);
static_assert(offsetof(EmbedderCell, state) == JS_LAYOUT_CELL_STATE_OFFSET);
static_assert(offsetof(EmbedderCell, kind) == JS_LAYOUT_CELL_KIND_OFFSET);

constexpr size_t cells_of_each_kind = 64;

enum class CellKind : u32 {
    HeldAsRealm,
    HeldAsScript,
    HeldByNothing,
};

bool s_finalized[3 * cells_of_each_kind];

u32 id_of(CellKind kind, size_t index)
{
    return static_cast<u32>(kind) * cells_of_each_kind + index;
}

size_t finalized_count_of(CellKind kind)
{
    size_t count = 0;
    for (size_t index = 0; index < cells_of_each_kind; ++index)
        count += s_finalized[id_of(kind, index)];
    return count;
}

// LibGC calls these through its own C++ function pointer types, which -fsanitize=function would reject.
#if defined(AK_COMPILER_CLANG)
#    define EMBEDDER_CALLBACK __attribute__((no_sanitize("function")))
#else
#    define EMBEDDER_CALLBACK
#endif

EMBEDDER_CALLBACK void visit_no_edges(GCCell*, GCVisitor*) { }

EMBEDDER_CALLBACK void record_finalization(GCCell* cell)
{
    s_finalized[reinterpret_cast<EmbedderCell*>(cell)->id] = true;
}

GCCellTypeInfo const s_embedder_cell_type_info {
    .cell_size = sizeof(EmbedderCell),
    .alignment = alignof(EmbedderCell),
    .kind = GC_CELL_KIND_OTHER,
    .visit_edges = visit_no_edges,
    .finalize = record_finalization,
    .destroy = nullptr,
    .external_memory_size = nullptr,
    .class_name = nullptr,
};

constexpr char s_allocator_name[] = "EmbedderCell";

// What owns the execution contexts, like LibWeb's environment settings objects own their realm execution contexts and
// visit them from their visit_edges().
struct OwnerOfExecutionContexts {
    GCHeap* heap { nullptr };
    GCAllocator* allocator { nullptr };
    Vector<JSExecutionContext*> execution_contexts;

    OwnerOfExecutionContexts()
    {
        __builtin_memset(s_finalized, 0, sizeof(s_finalized));
        heap = gc_heap_create(visit_execution_contexts, this, false);
        allocator = gc_allocator_create(&s_embedder_cell_type_info, s_allocator_name, sizeof(s_allocator_name) - 1);
    }

    ~OwnerOfExecutionContexts()
    {
        for (auto* execution_context : execution_contexts)
            js_execution_context_destroy(execution_context);
        gc_heap_destroy(heap);
        gc_allocator_destroy(allocator);
    }

    static void visit_execution_contexts(void* context, GCVisitor* visitor)
    {
        for (auto* execution_context : static_cast<OwnerOfExecutionContexts*>(context)->execution_contexts)
            js_execution_context_visit(execution_context, visitor);
    }

    GCCell* allocate(CellKind kind, size_t index)
    {
        bool must_mark = false;
        auto* cell = reinterpret_cast<EmbedderCell*>(gc_heap_allocate_cell(heap, allocator, &must_mark));
        *cell = {
            .class_word = &s_embedder_cell_type_info,
            .mark = must_mark,
            .state = GC_CELL_STATE_LIVE,
            .kind = GC_CELL_KIND_OTHER,
            .id = id_of(kind, index),
            .room_for_the_free_list_link = {},
        };
        return reinterpret_cast<GCCell*>(cell);
    }

    void collect() { gc_heap_collect_garbage(heap, GC_COLLECTION_TYPE_COLLECT_GARBAGE, false); }
};

NEVER_INLINE void create_execution_contexts_holding_new_cells(OwnerOfExecutionContexts& owner)
{
    for (size_t index = 0; index < cells_of_each_kind; ++index) {
        auto* execution_context = js_execution_context_create(0, 0, 0);
        realm_of(execution_context) = owner.allocate(CellKind::HeldAsRealm, index);
        script_or_module_of(execution_context) = {
            .tag = JS_LAYOUT_SCRIPT_OR_MODULE_TAG_SCRIPT,
            .cell = owner.allocate(CellKind::HeldAsScript, index),
        };
        owner.execution_contexts.append(execution_context);
        (void)owner.allocate(CellKind::HeldByNothing, index);
    }
}

NEVER_INLINE void scrub_stack()
{
    u8 volatile filler[8 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
}

}

TEST_CASE(an_owned_context_has_the_layout_that_the_layout_header_describes)
{
    auto* execution_context = js_execution_context_create(2, 1, 3);
    EXPECT_EQ(slot_count_of(execution_context), 6u);
    EXPECT_EQ(argument_count_of(execution_context), 3u);
    EXPECT(realm_of(execution_context) == nullptr);
    EXPECT_EQ(script_or_module_of(execution_context).tag, JS_LAYOUT_SCRIPT_OR_MODULE_TAG_EMPTY);

    auto* arguments = arguments_of(execution_context);
    for (size_t index = 0; index < argument_count_of(execution_context); ++index)
        arguments[index] = bit_cast<u64>(static_cast<double>(index) + 0.5);
    program_counter_of(execution_context) = 42;

    auto* copy = js_execution_context_copy(execution_context);
    EXPECT(copy != execution_context);
    EXPECT_EQ(slot_count_of(copy), 6u);
    EXPECT_EQ(argument_count_of(copy), 3u);
    EXPECT_EQ(program_counter_of(copy), 42u);
    for (size_t index = 0; index < argument_count_of(copy); ++index)
        EXPECT_EQ(arguments_of(copy)[index], bit_cast<u64>(static_cast<double>(index) + 0.5));

    js_execution_context_destroy(copy);
    js_execution_context_destroy(execution_context);
}

TEST_CASE(an_owned_context_keeps_the_cells_it_holds_alive_while_its_owner_visits_it)
{
    GCLayout layout;
    gc_get_layout(&layout);
    EXPECT_EQ(layout.cell_mark_offset, offsetof(EmbedderCell, mark));
    EXPECT_EQ(layout.cell_state_offset, offsetof(EmbedderCell, state));
    EXPECT_EQ(layout.cell_kind_offset, offsetof(EmbedderCell, kind));
    EXPECT(layout.min_cell_size <= sizeof(EmbedderCell));

    gc_heap_region_base();
    OwnerOfExecutionContexts owner;
    create_execution_contexts_holding_new_cells(owner);
    scrub_stack();
    owner.collect();
    EXPECT_EQ(finalized_count_of(CellKind::HeldAsRealm), 0u);
    EXPECT_EQ(finalized_count_of(CellKind::HeldAsScript), 0u);
    EXPECT(finalized_count_of(CellKind::HeldByNothing) >= cells_of_each_kind / 2);

    // The runtime reads the tag before the payload, so an empty ScriptOrModule holds nothing, whatever its payload says.
    for (auto* execution_context : owner.execution_contexts)
        script_or_module_of(execution_context).tag = JS_LAYOUT_SCRIPT_OR_MODULE_TAG_EMPTY;
    scrub_stack();
    owner.collect();
    EXPECT_EQ(finalized_count_of(CellKind::HeldAsRealm), 0u);
    EXPECT(finalized_count_of(CellKind::HeldAsScript) >= cells_of_each_kind / 2);
}

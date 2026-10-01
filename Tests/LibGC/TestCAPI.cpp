/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/StdLibExtras.h>
#include <LibGC/CAPI.h>
#include <LibGC/Cell.h>
#include <LibGC/HeapRegion.h>
#include <LibGC/NanBoxedValue.h>
#include <LibTest/TestCase.h>

namespace {

// A cell laid out the way a foreign implementation would lay it out: a word it owns, then the header bytes LibGC
// reads and writes, then its own fields.
struct ForeignCell {
    void const* class_word;
    bool mark;
    u8 state;
    u8 kind;
    ForeignCell* edge;
    u64 boxed_edge;
};

size_t s_finalized_cells = 0;
size_t s_destroyed_cells = 0;

// LibGC calls these through its own C++ function pointer types, which -fsanitize=function would reject.
#if defined(AK_COMPILER_CLANG)
#    define FOREIGN_CALLBACK __attribute__((no_sanitize("function")))
#else
#    define FOREIGN_CALLBACK
#endif

FOREIGN_CALLBACK void visit_foreign_cell_edges(GCCell* cell, GCVisitor* visitor)
{
    auto& foreign_cell = *reinterpret_cast<ForeignCell*>(cell);
    if (foreign_cell.edge)
        gc_visitor_visit_cell(visitor, reinterpret_cast<GCCell*>(foreign_cell.edge));
    gc_visitor_visit_values(visitor, &foreign_cell.boxed_edge, 1);
}

FOREIGN_CALLBACK void finalize_foreign_cell(GCCell*)
{
    ++s_finalized_cells;
}

FOREIGN_CALLBACK void destroy_foreign_cell(GCCell*)
{
    ++s_destroyed_cells;
}

FOREIGN_CALLBACK char const* foreign_cell_class_name(GCCell const*, size_t* length)
{
    static constexpr char name[] = "ForeignCell";
    *length = sizeof(name) - 1;
    return name;
}

GCCellTypeInfo const s_foreign_cell_type_info {
    .cell_size = sizeof(ForeignCell),
    .alignment = alignof(ForeignCell),
    .kind = GC_CELL_KIND_OTHER,
    .visit_edges = visit_foreign_cell_edges,
    .finalize = finalize_foreign_cell,
    .destroy = destroy_foreign_cell,
    .external_memory_size = nullptr,
    .class_name = foreign_cell_class_name,
};

constexpr char s_allocator_name[] = "ForeignCellAllocator";

struct TestHeap {
    GCHeap* heap { nullptr };
    GCAllocator* allocator { nullptr };
    ForeignCell* gathered_root { nullptr };

    explicit TestHeap(bool incremental_sweep)
    {
        s_finalized_cells = 0;
        s_destroyed_cells = 0;
        heap = gc_heap_create(gather_roots, this, false);
        gc_heap_set_incremental_sweep_enabled(heap, incremental_sweep);
        allocator = gc_allocator_create(&s_foreign_cell_type_info, s_allocator_name, sizeof(s_allocator_name) - 1);
    }

    ~TestHeap()
    {
        gc_heap_destroy(heap);
        gc_allocator_destroy(allocator);
    }

    static void gather_roots(void* context, GCVisitor* root_visitor)
    {
        auto& test_heap = *static_cast<TestHeap*>(context);
        if (test_heap.gathered_root)
            gc_visitor_visit_cell(root_visitor, reinterpret_cast<GCCell*>(test_heap.gathered_root));
    }

    ForeignCell* allocate(bool* must_mark = nullptr)
    {
        bool must_mark_storage = false;
        auto* cell = reinterpret_cast<ForeignCell*>(gc_heap_allocate_cell(heap, allocator, &must_mark_storage));
        *cell = {
            .class_word = &s_foreign_cell_type_info,
            .mark = must_mark_storage,
            .state = GC_CELL_STATE_LIVE,
            .kind = GC_CELL_KIND_OTHER,
            .edge = nullptr,
            .boxed_edge = GC::CANON_NAN_BITS,
        };
        if (must_mark)
            *must_mark = must_mark_storage;
        return cell;
    }

    void collect() { gc_heap_collect_garbage(heap, GC_COLLECTION_TYPE_COLLECT_GARBAGE, false); }
};

u64 box_cell(ForeignCell* cell)
{
    return GC::SHIFTED_IS_CELL_PATTERN | GC::NanBoxedValue::encode_pointer_bits(cell);
}

bool weak_impl_points_somewhere(GCWeakImpl* impl, GCLayout const& layout)
{
    void* pointer = nullptr;
    __builtin_memcpy(&pointer, reinterpret_cast<u8 const*>(impl) + layout.weak_impl_pointer_offset, sizeof(pointer));
    return pointer != nullptr;
}

NEVER_INLINE void scrub_stack()
{
    u8 volatile filler[8 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
}

struct Graph {
    GCWeakImpl* edge_target { nullptr };
    GCWeakImpl* boxed_edge_target { nullptr };
    GCWeakImpl* unreachable { nullptr };
};

NEVER_INLINE Graph allocate_graph(TestHeap& test_heap)
{
    auto* root = test_heap.allocate();
    auto* edge_target = test_heap.allocate();
    auto* boxed_edge_target = test_heap.allocate();
    auto* unreachable = test_heap.allocate();
    root->edge = edge_target;
    root->boxed_edge = box_cell(boxed_edge_target);
    test_heap.gathered_root = root;
    return {
        .edge_target = gc_heap_create_weak_impl(test_heap.heap, reinterpret_cast<GCCell*>(edge_target)),
        .boxed_edge_target = gc_heap_create_weak_impl(test_heap.heap, reinterpret_cast<GCCell*>(boxed_edge_target)),
        .unreachable = gc_heap_create_weak_impl(test_heap.heap, reinterpret_cast<GCCell*>(unreachable)),
    };
}

NEVER_INLINE GCRoot* allocate_rooted_cell(TestHeap& test_heap, GCWeakImpl** weak)
{
    auto* cell = test_heap.allocate();
    *weak = gc_heap_create_weak_impl(test_heap.heap, reinterpret_cast<GCCell*>(cell));
    return gc_root_create(reinterpret_cast<GCCell*>(cell));
}

NEVER_INLINE void allocate_garbage(TestHeap& test_heap, size_t count)
{
    for (size_t i = 0; i < count; ++i)
        (void)test_heap.allocate();
}

}

TEST_CASE(layout_describes_the_cell_header)
{
    GCLayout layout;
    gc_get_layout(&layout);

    EXPECT_EQ(layout.cell_mark_offset, offsetof(ForeignCell, mark));
    EXPECT_EQ(layout.cell_state_offset, offsetof(ForeignCell, state));
    EXPECT_EQ(layout.cell_kind_offset, offsetof(ForeignCell, kind));
    EXPECT(layout.min_cell_size <= sizeof(ForeignCell));
    EXPECT(layout.max_cell_size >= sizeof(ForeignCell));
    EXPECT_EQ(layout.cell_type_info_size, sizeof(GCCellTypeInfo));
    EXPECT_EQ(layout.heap_region_offset_mask, GC::HEAP_REGION_OFFSET_MASK);
}

TEST_CASE(region_base_is_the_nan_boxing_base)
{
    auto base = gc_heap_region_base();
    EXPECT(base != 0);
    EXPECT_EQ(base, js_heap_region_base);
}

TEST_CASE(stack_bounds_contain_the_current_frame)
{
    TestHeap test_heap { false };
    uintptr_t base = 0;
    uintptr_t top = 0;
    gc_heap_stack_bounds(test_heap.heap, &base, &top);
    auto frame = reinterpret_cast<uintptr_t>(__builtin_frame_address(0));
    EXPECT(base < frame);
    EXPECT(frame < top);
}

TEST_CASE(allocated_cells_use_their_type_info)
{
    TestHeap test_heap { false };
    auto* cell = test_heap.allocate();

    EXPECT_EQ(gc_cell_type_info(reinterpret_cast<GCCell*>(cell)), &s_foreign_cell_type_info);
    EXPECT_EQ(GC::class_name_of(*reinterpret_cast<GC::Cell*>(cell)), "ForeignCell"sv);
}

TEST_CASE(gathered_roots_and_their_edges_survive_collection)
{
    TestHeap test_heap { false };
    GCLayout layout;
    gc_get_layout(&layout);

    auto graph = allocate_graph(test_heap);
    scrub_stack();
    test_heap.collect();

    EXPECT(weak_impl_points_somewhere(graph.edge_target, layout));
    EXPECT(weak_impl_points_somewhere(graph.boxed_edge_target, layout));
    EXPECT(!weak_impl_points_somewhere(graph.unreachable, layout));
    EXPECT_EQ(s_finalized_cells, 1u);
    EXPECT_EQ(s_destroyed_cells, 1u);

    test_heap.gathered_root = nullptr;
    scrub_stack();
    test_heap.collect();

    EXPECT(!weak_impl_points_somewhere(graph.edge_target, layout));
    EXPECT(!weak_impl_points_somewhere(graph.boxed_edge_target, layout));
    EXPECT_EQ(s_finalized_cells, 4u);
    EXPECT_EQ(s_destroyed_cells, 4u);

    gc_weak_impl_unref(graph.edge_target);
    gc_weak_impl_unref(graph.boxed_edge_target);
    gc_weak_impl_unref(graph.unreachable);
}

TEST_CASE(roots_keep_cells_alive_until_destroyed)
{
    TestHeap test_heap { false };
    GCLayout layout;
    gc_get_layout(&layout);

    GCWeakImpl* weak = nullptr;
    auto* root = allocate_rooted_cell(test_heap, &weak);
    scrub_stack();
    test_heap.collect();
    EXPECT(weak_impl_points_somewhere(weak, layout));
    EXPECT_EQ(gc_cell_type_info(gc_root_cell(root)), &s_foreign_cell_type_info);

    gc_root_destroy(root);
    scrub_stack();
    test_heap.collect();
    EXPECT(!weak_impl_points_somewhere(weak, layout));

    gc_weak_impl_unref(weak);
}

TEST_CASE(null_weak_impl_points_nowhere)
{
    GCLayout layout;
    gc_get_layout(&layout);
    auto* null_weak_impl = gc_weak_impl_null();
    EXPECT(!weak_impl_points_somewhere(null_weak_impl, layout));
    gc_weak_impl_unref(null_weak_impl);
}

TEST_CASE(cells_allocated_during_incremental_sweep_start_out_marked)
{
    TestHeap test_heap { true };
    bool must_mark = true;
    (void)test_heap.allocate(&must_mark);
    EXPECT(!must_mark);

    allocate_garbage(test_heap, 1000);
    test_heap.collect();

    auto* cell = test_heap.allocate(&must_mark);
    EXPECT(must_mark);
    EXPECT(cell->mark);
}

TEST_CASE(sweep_callbacks_run_every_collection_and_post_gc_tasks_once)
{
    // NB: Declared before the heap, since its destruction runs the sweep callback once more.
    size_t sweep_callback_runs = 0;
    size_t post_gc_task_runs = 0;
    TestHeap test_heap { false };
    gc_heap_register_sweep_callback(test_heap.heap, [](void* context) { ++*static_cast<size_t*>(context); }, &sweep_callback_runs);
    gc_heap_enqueue_post_gc_task(test_heap.heap, [](void* context) { ++*static_cast<size_t*>(context); }, &post_gc_task_runs);

    test_heap.collect();
    EXPECT_EQ(sweep_callback_runs, 1u);
    EXPECT_EQ(post_gc_task_runs, 1u);

    test_heap.collect();
    EXPECT_EQ(sweep_callback_runs, 2u);
    EXPECT_EQ(post_gc_task_runs, 1u);
}

TEST_CASE(collection_waits_for_deferral_to_end)
{
    size_t sweep_callback_runs = 0;
    TestHeap test_heap { false };
    gc_heap_register_sweep_callback(test_heap.heap, [](void* context) { ++*static_cast<size_t*>(context); }, &sweep_callback_runs);

    gc_heap_defer_gc(test_heap.heap);
    test_heap.collect();
    EXPECT_EQ(sweep_callback_runs, 0u);
    gc_heap_undefer_gc(test_heap.heap);
    EXPECT_EQ(sweep_callback_runs, 1u);
}

TEST_CASE(heap_destruction_destroys_every_cell)
{
    {
        TestHeap test_heap { false };
        allocate_garbage(test_heap, 100);
        test_heap.gathered_root = test_heap.allocate();
    }
    EXPECT_EQ(s_destroyed_cells, 101u);
    EXPECT_EQ(s_finalized_cells, 101u);
}

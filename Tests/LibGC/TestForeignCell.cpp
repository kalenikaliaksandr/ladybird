/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/ByteString.h>
#include <AK/HashTable.h>
#include <AK/Vector.h>
#include <LibGC/CAPI.h>
#include <LibGC/Cell.h>
#include <LibGC/CellAllocator.h>
#include <LibGC/CellTypeInfo.h>
#include <LibGC/CrossHeapMember.h>
#include <LibGC/Heap.h>
#include <LibGC/HeapGroup.h>
#include <LibGC/Ptr.h>
#include <LibGC/Root.h>
#include <LibGC/RootVector.h>
#include <LibGC/Weak.h>
#include <LibGC/WeakHashMap.h>
#include <LibGC/WeakInlines.h>
#include <LibTest/TestCase.h>

namespace {

// A cell as its foreign implementation lays it out, the way the Rust runtime lays out its cells: the word that
// implementation owns, the header bytes LibGC reads and writes, then the cell's own fields.
struct ForeignNodeLayout {
    void const* class_word;
    bool mark;
    u8 state;
    u8 kind;
    u32 id;
    GCCell* edge;
};

// How C++ code names these cells, as the LibJS facade names the Rust runtime's: a type with no fields of its own,
// whose pointers are the foreign cells themselves.
class ForeignNode : public GC::ForeignCell {
public:
    u32 id() const { return layout().id; }
    void set_edge(GC::Cell& cell) { layout().edge = reinterpret_cast<GCCell*>(&cell); }

private:
    ForeignNodeLayout const& layout() const { return *reinterpret_cast<ForeignNodeLayout const*>(this); }
    ForeignNodeLayout& layout() { return *reinterpret_cast<ForeignNodeLayout*>(this); }
};

// Types that would put their own fields or a vtable pointer over the foreign cell's.
struct ForeignNodeWithAField : public GC::ForeignCell {
    u32 cached_id;
};

struct ForeignNodeWithAVtable : public GC::ForeignCell {
    virtual void method_of_the_vtable() { }
};

static_assert(GC::IsForeignCellHandle<ForeignNode>);
static_assert(GC::IsForeignCellHandle<ForeignNode const>);
static_assert(!GC::IsForeignCellHandle<ForeignNodeWithAField>);
static_assert(!GC::IsForeignCellHandle<ForeignNodeWithAVtable>);

HashTable<u32> s_finalized_node_ids;
size_t s_destroyed_leaf_count = 0;

// LibGC calls these through its own C++ function pointer types, which -fsanitize=function would reject.
#if defined(AK_COMPILER_CLANG)
#    define FOREIGN_CALLBACK __attribute__((no_sanitize("function")))
#else
#    define FOREIGN_CALLBACK
#endif

FOREIGN_CALLBACK void visit_foreign_node_edges(GCCell* cell, GCVisitor* visitor)
{
    if (auto* edge = reinterpret_cast<ForeignNodeLayout*>(cell)->edge)
        gc_visitor_visit_cell(visitor, edge);
}

FOREIGN_CALLBACK void record_foreign_node_finalization(GCCell* cell)
{
    s_finalized_node_ids.set(reinterpret_cast<ForeignNodeLayout*>(cell)->id);
}

FOREIGN_CALLBACK char const* foreign_node_class_name(GCCell const*, size_t* length)
{
    static constexpr char name[] = "ForeignNode";
    *length = sizeof(name) - 1;
    return name;
}

GCCellTypeInfo const s_foreign_node_type_info {
    .cell_size = sizeof(ForeignNodeLayout),
    .alignment = alignof(ForeignNodeLayout),
    .kind = GC_CELL_KIND_OBJECT,
    .visit_edges = visit_foreign_node_edges,
    .finalize = record_foreign_node_finalization,
    .destroy = nullptr,
    .external_memory_size = nullptr,
    .class_name = foreign_node_class_name,
};

constexpr char s_foreign_node_allocator_name[] = "ForeignNode";

// A C++ cell that a foreign node can hold.
class Leaf final : public GC::Cell {
    GC_CELL(Leaf, GC::Cell);
    GC_DECLARE_ALLOCATOR(Leaf);

public:
    virtual ~Leaf() override { ++s_destroyed_leaf_count; }

private:
    Leaf() = default;

    [[maybe_unused]] FlatPtr m_room_for_a_freelist_entry[2] {};
};

GC_DEFINE_ALLOCATOR(Leaf);

// A C++ cell that holds foreign nodes in every way C++ cells hold cells.
class NodeHolder final : public GC::Cell {
    GC_CELL(NodeHolder, GC::Cell);
    GC_DECLARE_ALLOCATOR(NodeHolder);

public:
    GC::Ptr<ForeignNode>& node_held_through_ptr() { return m_node_held_through_ptr; }
    GC::CrossHeapMember<ForeignNode>& node_on_another_heap() { return m_node_on_another_heap; }

private:
    explicit NodeHolder(ForeignNode& node_held_through_ref)
        : m_node_held_through_ref(node_held_through_ref)
    {
    }

    virtual void visit_edges(Visitor& visitor) override
    {
        Base::visit_edges(visitor);
        visitor.visit(m_node_held_through_ptr);
        visitor.visit(m_node_held_through_ref);
        m_node_on_another_heap.visit(visitor);
    }

    GC::Ptr<ForeignNode> m_node_held_through_ptr;
    GC::Ref<ForeignNode> m_node_held_through_ref;
    GC::CrossHeapMember<ForeignNode> m_node_on_another_heap;
};

GC_DEFINE_ALLOCATOR(NodeHolder);

// A heap that is created and allocated from through the C API, like the Rust runtime's, and that C++ code uses as a
// GC::Heap.
class ForeignHeap {
public:
    ForeignHeap()
        : m_heap(gc_heap_create(gather_no_roots, nullptr, true))
        , m_node_allocator(gc_allocator_create(&s_foreign_node_type_info, s_foreign_node_allocator_name, sizeof(s_foreign_node_allocator_name) - 1))
    {
        gc_heap_set_incremental_sweep_enabled(m_heap, false);
        s_finalized_node_ids.clear();
        s_destroyed_leaf_count = 0;
    }

    ~ForeignHeap()
    {
        gc_heap_destroy(m_heap);
        gc_allocator_destroy(m_node_allocator);
    }

    GC::Heap& heap() const { return *reinterpret_cast<GC::Heap*>(m_heap); }

    ForeignNode& allocate_node(u32 id)
    {
        bool must_mark = false;
        auto* cell = gc_heap_allocate_cell(m_heap, m_node_allocator, &must_mark);
        *reinterpret_cast<ForeignNodeLayout*>(cell) = {
            .class_word = &s_foreign_node_type_info,
            .mark = must_mark,
            .state = GC_CELL_STATE_LIVE,
            .kind = GC_CELL_KIND_OBJECT,
            .id = id,
            .edge = nullptr,
        };
        return *reinterpret_cast<ForeignNode*>(cell);
    }

    void collect() { heap().collect_garbage(); }

private:
    static void gather_no_roots(void*, GCVisitor*) { }

    GCHeap* m_heap { nullptr };
    GCAllocator* m_node_allocator { nullptr };
};

class RecordingVisitor final : public GC::Cell::Visitor {
public:
    Vector<GC::Cell*> visited_cells;

private:
    virtual void visit_impl(GC::Cell& cell) override { visited_cells.append(&cell); }
    virtual void visit_impl(ReadonlySpan<GC::NanBoxedValue>) override { }
    virtual void visit_possible_values(ReadonlyBytes) override { }
};

template<typename Visitable>
Vector<GC::Cell*> cells_visited_through(Visitable&& visitable)
{
    RecordingVisitor visitor;
    visitor.visit(visitable);
    return move(visitor.visited_cells);
}

NEVER_INLINE void scrub_stack()
{
    u8 volatile filler[8 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
}

void collect_with_a_clean_stack(ForeignHeap& foreign_heap)
{
    scrub_stack();
    foreign_heap.collect();
}

bool node_was_finalized(u32 id)
{
    return s_finalized_node_ids.contains(id);
}

NEVER_INLINE GC::Root<NodeHolder> allocate_cells_held_by_each_other(ForeignHeap& foreign_heap)
{
    auto& node_held_through_ptr = foreign_heap.allocate_node(1);
    auto& node_held_through_ref = foreign_heap.allocate_node(2);
    (void)foreign_heap.allocate_node(3);
    node_held_through_ref.set_edge(*foreign_heap.heap().allocate<Leaf>());

    auto holder = foreign_heap.heap().allocate<NodeHolder>(node_held_through_ref);
    holder->node_held_through_ptr() = node_held_through_ptr;
    return GC::make_root(holder);
}

struct RootedNodes {
    GC::Root<ForeignNode> root;
    GC::Root<ForeignNode> root_made_from_ptr;
    GC::RootVector<GC::Ref<ForeignNode>> vector_of_refs;
    GC::RootVector<ForeignNode*> vector_of_pointers;
};

NEVER_INLINE RootedNodes allocate_rooted_nodes(ForeignHeap& foreign_heap)
{
    RootedNodes rooted_nodes {
        .root = foreign_heap.allocate_node(1),
        .root_made_from_ptr = GC::make_root(GC::Ptr<ForeignNode> { foreign_heap.allocate_node(2) }),
        .vector_of_refs = {},
        .vector_of_pointers = {},
    };
    rooted_nodes.vector_of_refs.append(foreign_heap.allocate_node(3));
    rooted_nodes.vector_of_pointers.append(&foreign_heap.allocate_node(4));
    (void)foreign_heap.allocate_node(5);
    return rooted_nodes;
}

struct WeaklyHeldNode {
    GC::Root<ForeignNode> root;
    GC::Weak<ForeignNode> weak_from_reference;
    GC::Weak<ForeignNode> weak_from_pointer;
    GC::Weak<ForeignNode> weak_from_ptr;
    GC::Weak<ForeignNode> weak_from_ref;
    GC::Weak<GC::ForeignCell> weak_to_base_from_weak;
};

NEVER_INLINE WeaklyHeldNode allocate_weakly_held_node(ForeignHeap& foreign_heap)
{
    auto& node = foreign_heap.allocate_node(1);
    GC::Weak<ForeignNode> weak_from_reference { node };
    return {
        .root = node,
        .weak_from_reference = weak_from_reference,
        .weak_from_pointer = &node,
        .weak_from_ptr = GC::Ptr<ForeignNode> { node },
        .weak_from_ref = GC::Ref<ForeignNode> { node },
        .weak_to_base_from_weak = weak_from_reference,
    };
}

// NB: Comparisons keep their operands in the frame they run in, where the conservative scan would find the node
//     after its root is gone. Running them in a frame of their own lets scrub_stack() wipe them.
NEVER_INLINE void expect_weak_references_to_point_at_the_rooted_node(WeaklyHeldNode const& weakly_held_node)
{
    EXPECT_EQ(weakly_held_node.weak_from_reference.ptr().ptr(), weakly_held_node.root.ptr());
    EXPECT_EQ(weakly_held_node.weak_from_pointer.ptr().ptr(), weakly_held_node.root.ptr());
    EXPECT_EQ(weakly_held_node.weak_from_ptr.ptr().ptr(), weakly_held_node.root.ptr());
    EXPECT_EQ(weakly_held_node.weak_from_ref.ptr().ptr(), weakly_held_node.root.ptr());
    EXPECT_EQ(weakly_held_node.weak_to_base_from_weak.ptr().ptr(), weakly_held_node.root.ptr());
}

NEVER_INLINE GC::Root<ForeignNode> fill_weak_hash_maps(ForeignHeap& foreign_heap, GC::WeakHashMap<ForeignNode, u32>& ids_by_node, GC::WeakHashMap<u32, ForeignNode>& nodes_by_id)
{
    for (u32 id = 1; id <= 2; ++id) {
        auto& node = foreign_heap.allocate_node(id);
        ids_by_node.set(node, id);
        nodes_by_id.set(id, node);
    }
    return *nodes_by_id.get(1);
}

NEVER_INLINE void expect_weak_hash_maps_to_hold_only_the_rooted_node(GC::WeakHashMap<ForeignNode, u32>& ids_by_node, GC::WeakHashMap<u32, ForeignNode>& nodes_by_id, GC::Root<ForeignNode> const& rooted_node)
{
    Vector<u32> ids_of_live_nodes;
    ids_by_node.for_each_live_value([&](u32 id) { ids_of_live_nodes.append(id); });
    EXPECT_EQ(ids_of_live_nodes, Vector<u32> { 1 });
    EXPECT_EQ(ids_by_node.get(*rooted_node).value(), 1u);
    EXPECT_EQ(nodes_by_id.get(1), rooted_node.ptr());
    EXPECT(!nodes_by_id.get(2));
}

NEVER_INLINE GC::Root<NodeHolder> allocate_holder_of_two_nodes(ForeignHeap& foreign_heap)
{
    auto holder = foreign_heap.heap().allocate<NodeHolder>(foreign_heap.allocate_node(1));
    holder->node_held_through_ptr() = foreign_heap.allocate_node(2);
    return GC::make_root(holder);
}

NEVER_INLINE GC::Root<NodeHolder> allocate_holder_of_node_on_another_heap(ForeignHeap& holder_heap, ForeignHeap& node_heap)
{
    auto holder = holder_heap.heap().allocate<NodeHolder>(holder_heap.allocate_node(1));
    holder->node_on_another_heap() = &node_heap.allocate_node(2);
    return GC::make_root(holder);
}

}

TEST_CASE(foreign_cells_read_their_header_and_heap_like_cells)
{
    ForeignHeap foreign_heap;
    auto& node = foreign_heap.allocate_node(1);

    EXPECT_EQ(&node.heap(), &foreign_heap.heap());
    EXPECT_EQ(&node.type_info(), reinterpret_cast<GC::CellTypeInfo const*>(&s_foreign_node_type_info));
    EXPECT(node.cell_kind() == GC::CellKind::Object);
    EXPECT(node.state() == GC::Cell::State::Live);
    EXPECT(!node.is_marked());
    EXPECT_EQ(node.id(), 1u);

    EXPECT_EQ(GC::as_cell(&node), reinterpret_cast<GC::Cell*>(&node));
    EXPECT_EQ(GC::static_cell_cast<ForeignNode>(GC::as_cell(&node)), &node);
    EXPECT_EQ(GC::class_name_of(node), "ForeignNode"sv);
    GC::ForeignCell const& foreign_cell = node;
    EXPECT_EQ(ByteString::formatted("{}", foreign_cell), ByteString::formatted("ForeignNode({})", &foreign_cell));
}

TEST_CASE(pointers_to_foreign_cells_convert_like_pointers_to_cells)
{
    ForeignHeap foreign_heap;
    auto& node = foreign_heap.allocate_node(1);

    GC::Ref<ForeignNode> ref = node;
    GC::Ptr<ForeignNode> ptr = ref;
    GC::Ptr<GC::ForeignCell> base_ptr = ptr;
    GC::Ref<ForeignNode const> const_ref = ref;

    EXPECT(ptr == ref);
    EXPECT(base_ptr == ref);
    EXPECT_EQ(const_ref.ptr(), &node);
    EXPECT_EQ(base_ptr.ptr(), static_cast<GC::ForeignCell*>(&node));
}

TEST_CASE(pointers_to_cells_accept_foreign_cells)
{
    ForeignHeap foreign_heap;
    auto& node = foreign_heap.allocate_node(1);
    auto& other_node = foreign_heap.allocate_node(2);
    auto const& const_node = node;
    auto* node_as_cell = GC::as_cell(&node);
    auto* other_node_as_cell = GC::as_cell(&other_node);

    GC::Ref<GC::Cell> ref_from_reference { node };
    GC::Ref<GC::Cell> ref_from_ref { GC::Ref<ForeignNode> { node } };
    GC::Ref<GC::Cell const> const_ref_from_const_reference { const_node };
    GC::Ptr<GC::Cell> ptr_from_reference { node };
    GC::Ptr<GC::Cell> ptr_from_pointer { &node };
    GC::Ptr<GC::Cell> ptr_from_ptr { GC::Ptr<ForeignNode> { node } };
    GC::Ptr<GC::Cell> ptr_from_ref { GC::Ref<ForeignNode> { node } };
    GC::Ptr<GC::Cell> ptr_from_null_ptr { GC::Ptr<ForeignNode> {} };
    GC::Ptr<GC::Cell const> const_ptr_from_const_pointer { &const_node };

    EXPECT_EQ(ref_from_reference.ptr(), node_as_cell);
    EXPECT_EQ(ref_from_ref.ptr(), node_as_cell);
    EXPECT_EQ(const_ref_from_const_reference.ptr(), node_as_cell);
    EXPECT_EQ(ptr_from_reference.ptr(), node_as_cell);
    EXPECT_EQ(ptr_from_pointer.ptr(), node_as_cell);
    EXPECT_EQ(ptr_from_ptr.ptr(), node_as_cell);
    EXPECT_EQ(ptr_from_ref.ptr(), node_as_cell);
    EXPECT(!ptr_from_null_ptr);
    EXPECT_EQ(const_ptr_from_const_pointer.ptr(), node_as_cell);

    GC::Ref<GC::Cell> assigned_ref { other_node };
    assigned_ref = node;
    EXPECT_EQ(assigned_ref.ptr(), node_as_cell);
    assigned_ref = GC::Ref<ForeignNode> { other_node };
    EXPECT_EQ(assigned_ref.ptr(), other_node_as_cell);

    GC::Ptr<GC::Cell> assigned_ptr;
    assigned_ptr = node;
    EXPECT_EQ(assigned_ptr.ptr(), node_as_cell);
    assigned_ptr = &other_node;
    EXPECT_EQ(assigned_ptr.ptr(), other_node_as_cell);
    assigned_ptr = GC::Ptr<ForeignNode> { node };
    EXPECT_EQ(assigned_ptr.ptr(), node_as_cell);
    assigned_ptr = GC::Ref<ForeignNode> { other_node };
    EXPECT_EQ(assigned_ptr.ptr(), other_node_as_cell);
    assigned_ptr = static_cast<ForeignNode*>(nullptr);
    EXPECT(!assigned_ptr);

    EXPECT(ref_from_reference == GC::Ref<ForeignNode> { node });
    EXPECT(GC::Ref<ForeignNode> { node } == ref_from_reference);
    EXPECT(ptr_from_pointer == GC::Ptr<ForeignNode> { node });
    EXPECT(GC::Ptr<ForeignNode> { node } == ptr_from_pointer);
    EXPECT(ptr_from_pointer == GC::Ref<ForeignNode> { node });
    EXPECT(GC::Ref<ForeignNode> { node } == ptr_from_pointer);
    EXPECT(const_ptr_from_const_pointer == GC::Ptr<ForeignNode> { node });
    EXPECT(ref_from_reference != GC::Ptr<ForeignNode> { other_node });
    EXPECT(ptr_from_null_ptr == GC::Ptr<ForeignNode> {});

    static_assert(!IsConstructible<GC::Ref<GC::Cell>, ForeignNode const&>);
    static_assert(!IsConstructible<GC::Ptr<GC::Cell>, ForeignNode const*>);
    static_assert(!IsConvertible<ForeignNode*, GC::Cell*>);
}

TEST_CASE(visitors_visit_foreign_cells_as_cells)
{
    ForeignHeap foreign_heap;
    auto& node = foreign_heap.allocate_node(1);
    Vector<GC::Cell*> only_the_node { GC::as_cell(&node) };

    EXPECT_EQ(cells_visited_through(&node), only_the_node);
    EXPECT_EQ(cells_visited_through(node), only_the_node);
    EXPECT_EQ(cells_visited_through(static_cast<ForeignNode const*>(&node)), only_the_node);
    EXPECT_EQ(cells_visited_through(static_cast<ForeignNode const&>(node)), only_the_node);
    EXPECT_EQ(cells_visited_through(GC::Ptr<ForeignNode> { node }), only_the_node);
    EXPECT_EQ(cells_visited_through(GC::Ptr<ForeignNode const> { node }), only_the_node);
    EXPECT_EQ(cells_visited_through(GC::Ref<ForeignNode> { node }), only_the_node);
    EXPECT_EQ(cells_visited_through(Vector<GC::Ref<ForeignNode>> { node }), only_the_node);

    EXPECT(cells_visited_through(static_cast<ForeignNode*>(nullptr)).is_empty());
    EXPECT(cells_visited_through(GC::Ptr<ForeignNode> {}).is_empty());
}

TEST_CASE(cpp_cells_and_foreign_cells_keep_each_other_alive)
{
    ForeignHeap foreign_heap;
    auto holder = allocate_cells_held_by_each_other(foreign_heap);

    collect_with_a_clean_stack(foreign_heap);
    EXPECT(!node_was_finalized(1));
    EXPECT(!node_was_finalized(2));
    EXPECT(node_was_finalized(3));
    EXPECT_EQ(s_destroyed_leaf_count, 0u);
    EXPECT_EQ(holder->node_held_through_ptr()->id(), 1u);

    holder = {};
    collect_with_a_clean_stack(foreign_heap);
    EXPECT(node_was_finalized(1));
    EXPECT(node_was_finalized(2));
    EXPECT_EQ(s_destroyed_leaf_count, 1u);
}

TEST_CASE(roots_keep_foreign_cells_alive_until_released)
{
    ForeignHeap foreign_heap;
    auto rooted_nodes = allocate_rooted_nodes(foreign_heap);

    collect_with_a_clean_stack(foreign_heap);
    EXPECT_EQ(s_finalized_node_ids.size(), 1u);
    EXPECT(node_was_finalized(5));
    EXPECT_EQ(rooted_nodes.root->id(), 1u);
    EXPECT_EQ(rooted_nodes.root_made_from_ptr.ptr()->id(), 2u);

    rooted_nodes.root = {};
    collect_with_a_clean_stack(foreign_heap);
    EXPECT(node_was_finalized(1));
    EXPECT_EQ(s_finalized_node_ids.size(), 2u);

    rooted_nodes.root_made_from_ptr = {};
    collect_with_a_clean_stack(foreign_heap);
    EXPECT(node_was_finalized(2));
    EXPECT_EQ(s_finalized_node_ids.size(), 3u);

    rooted_nodes.vector_of_refs.clear();
    collect_with_a_clean_stack(foreign_heap);
    EXPECT(node_was_finalized(3));
    EXPECT_EQ(s_finalized_node_ids.size(), 4u);

    rooted_nodes.vector_of_pointers.clear();
    collect_with_a_clean_stack(foreign_heap);
    EXPECT(node_was_finalized(4));
}

TEST_CASE(weak_references_to_foreign_cells_clear_once_collected)
{
    ForeignHeap foreign_heap;
    auto weakly_held_node = allocate_weakly_held_node(foreign_heap);

    collect_with_a_clean_stack(foreign_heap);
    EXPECT(!node_was_finalized(1));
    expect_weak_references_to_point_at_the_rooted_node(weakly_held_node);

    weakly_held_node.root = {};
    collect_with_a_clean_stack(foreign_heap);
    EXPECT(node_was_finalized(1));
    EXPECT(!weakly_held_node.weak_from_reference);
    EXPECT(!weakly_held_node.weak_from_pointer);
    EXPECT(!weakly_held_node.weak_from_ptr);
    EXPECT(!weakly_held_node.weak_from_ref);
    EXPECT(!weakly_held_node.weak_to_base_from_weak);
}

TEST_CASE(weak_hash_maps_drop_foreign_cells_once_collected)
{
    ForeignHeap foreign_heap;
    GC::WeakHashMap<ForeignNode, u32> ids_by_node;
    GC::WeakHashMap<u32, ForeignNode> nodes_by_id;
    auto rooted_node = fill_weak_hash_maps(foreign_heap, ids_by_node, nodes_by_id);

    collect_with_a_clean_stack(foreign_heap);
    EXPECT(node_was_finalized(2));
    EXPECT(!node_was_finalized(1));
    expect_weak_hash_maps_to_hold_only_the_rooted_node(ids_by_node, nodes_by_id, rooted_node);

    rooted_node = {};
    collect_with_a_clean_stack(foreign_heap);
    EXPECT(node_was_finalized(1));
    EXPECT(ids_by_node.is_empty());
    EXPECT(nodes_by_id.is_empty());
}

TEST_CASE(uprooted_foreign_cells_are_collected_while_still_referenced)
{
    ForeignHeap foreign_heap;
    auto holder = allocate_holder_of_two_nodes(foreign_heap);

    foreign_heap.heap().uproot_cell(holder->node_held_through_ptr().ptr());
    collect_with_a_clean_stack(foreign_heap);
    EXPECT(!node_was_finalized(1));
    EXPECT(node_was_finalized(2));
    holder->node_held_through_ptr() = nullptr;

    collect_with_a_clean_stack(foreign_heap);
    EXPECT(!node_was_finalized(1));
}

TEST_CASE(cross_heap_members_keep_foreign_cells_on_another_heap_alive)
{
    ForeignHeap holder_heap;
    ForeignHeap node_heap;
    GC::HeapGroup group;
    group.add(holder_heap.heap());
    group.add(node_heap.heap());

    auto holder = allocate_holder_of_node_on_another_heap(holder_heap, node_heap);
    collect_with_a_clean_stack(node_heap);
    EXPECT(!node_was_finalized(2));
    EXPECT_EQ(holder->node_on_another_heap()->id(), 2u);

    holder->node_on_another_heap() = nullptr;
    collect_with_a_clean_stack(node_heap);
    EXPECT(node_was_finalized(2));

    holder = {};
    group.remove(holder_heap.heap());
    group.remove(node_heap.heap());
}

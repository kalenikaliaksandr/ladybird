/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Array.h>
#include <AK/BitCast.h>
#include <AK/Vector.h>
#include <LibGC/CAPI.h>
#include <LibGC/Cell.h>
#include <LibGC/CellAllocator.h>
#include <LibGC/Heap.h>

#if !defined(AK_OS_WINDOWS)
#    include <unistd.h>
#endif

#include "EmbeddingTest.h"

namespace {

constexpr size_t storage_owner_count = 64;
Array<bool, storage_owner_count> s_storage_owner_was_finalized {};

// Like a WebAssembly.Memory or an AudioBuffer: a C++ cell that owns storage, which ArrayBuffers of the runtime view.
class StorageOwner final : public GC::Cell {
    GC_CELL(StorageOwner, GC::Cell);
    GC_DECLARE_ALLOCATOR(StorageOwner);

public:
    StorageOwner(size_t index, GCPrimitiveStorageHandle storage)
        : m_index(index)
        , m_storage(storage)
    {
    }

private:
    virtual void finalize() override
    {
        Base::finalize();
        s_storage_owner_was_finalized[m_index] = true;
        gc_primitive_storage_free(m_storage);
    }

    size_t m_index { 0 };
    GCPrimitiveStorageHandle m_storage { GC_PRIMITIVE_STORAGE_NULL_HANDLE };
};

GC_DEFINE_ALLOCATOR(StorageOwner);

JSExternalPrimitiveStorage external_storage(StorageOwner& owner, GCPrimitiveStorageHandle handle, Optional<size_t> fixed_byte_length)
{
    return {
        .owner = &owner,
        .handle = handle,
        .fixed_byte_length = fixed_byte_length.value_or(0),
        .has_fixed_byte_length = fixed_byte_length.has_value(),
    };
}

JSArrayBufferStorage storage_of(JSObject* buffer)
{
    JSArrayBufferStorage storage {};
    js_array_buffer_storage(buffer, &storage);
    return storage;
}

NEVER_INLINE void scrub_stack()
{
    u8 volatile filler[16 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
}

}

TEST_CASE(a_block_that_cpp_writes_is_what_a_uint8_array_of_the_runtime_views)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    GCPrimitiveStorageHandle handle = GC_PRIMITIVE_STORAGE_NULL_HANDLE;
    EXPECT(gc_primitive_storage_allocate(64, false, &handle, nullptr));
    auto* bytes = gc_primitive_storage_data(handle);
    for (u8 i = 0; i < 64; ++i)
        bytes[i] = i * 3;
    auto owner = GC::Heap::the().allocate<StorageOwner>(0, handle);

    auto storage = external_storage(*owner, handle, 64);
    auto* buffer = js_array_buffer_create_with_external_storage(embedded_vm->vm(), embedded_vm->realm(), &storage, false);
    JSByteLength sixteen { .length = 16, .kind = JS_BYTE_LENGTH_LENGTH };
    auto* view = js_typed_array_create_from_slots(embedded_vm->vm(), embedded_vm->realm(), JS_LAYOUT_TYPED_ARRAY_KIND_UINT8, buffer, sixteen, sixteen, 8);

    EXPECT_EQ(js_typed_array_kind(view), JS_LAYOUT_TYPED_ARRAY_KIND_UINT8);
    EXPECT_EQ(js_typed_array_element_size(view), 1u);
    EXPECT_EQ(js_typed_array_viewed_array_buffer(view), buffer);
    EXPECT_EQ(js_typed_array_byte_offset(view), 8u);
    EXPECT_EQ(js_typed_array_array_length(view).length, 16u);
    EXPECT_EQ(js_typed_array_array_length(view).kind, JS_BYTE_LENGTH_LENGTH);

    auto const* view_cell = reinterpret_cast<u8 const*>(view);
    EXPECT_EQ(view_cell[JS_LAYOUT_TYPED_ARRAY_KIND_OFFSET], JS_LAYOUT_TYPED_ARRAY_KIND_UINT8);
    EXPECT_EQ(*reinterpret_cast<u32 const*>(view_cell + JS_LAYOUT_TYPED_ARRAY_BYTE_OFFSET_OFFSET), 8u);
    EXPECT_EQ(*reinterpret_cast<u32 const*>(view_cell + JS_LAYOUT_TYPED_ARRAY_ARRAY_LENGTH_OFFSET), 16u);

    auto record = js_typed_array_make_witness_record(view, JS_ARRAY_BUFFER_ORDER_SEQ_CST);
    EXPECT(!js_typed_array_is_out_of_bounds(&record));
    EXPECT_EQ(js_typed_array_length_of_witness(&record), 16u);
    EXPECT_EQ(js_typed_array_byte_length_of_witness(&record), 16u);

    size_t byte_length = 0;
    auto* data = js_array_buffer_data(buffer, &byte_length);
    EXPECT_EQ(data, bytes);
    EXPECT_EQ(byte_length, 64u);
    EXPECT_EQ(data[js_typed_array_byte_offset(view) + 15], 69);
    bytes[8] = 200;
    EXPECT_EQ(data[js_typed_array_byte_offset(view)], 200);

    EXPECT(js_array_buffer_is_fixed_length(buffer));
    EXPECT(!js_array_buffer_is_shared_array_buffer(buffer));
    auto storage_description = storage_of(buffer);
    EXPECT_EQ(storage_description.kind, JS_ARRAY_BUFFER_STORAGE_EXTERNAL);
    EXPECT_EQ(storage_description.handle, handle);
    EXPECT_EQ(storage_description.external.owner, static_cast<void*>(owner.ptr()));
    EXPECT(storage_description.external.has_fixed_byte_length);
    EXPECT_EQ(storage_description.external.fixed_byte_length, 64u);
}

TEST_CASE(a_buffer_without_a_fixed_length_follows_the_storage_its_owner_resizes)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    GCPrimitiveStorageHandle handle = GC_PRIMITIVE_STORAGE_NULL_HANDLE;
    EXPECT(gc_primitive_storage_reserve(16, 64 * KiB, true, 0, &handle, nullptr));
    auto owner = GC::Heap::the().allocate<StorageOwner>(0, handle);
    auto storage = external_storage(*owner, handle, {});
    auto* buffer = js_array_buffer_create_with_external_storage(embedded_vm->vm(), embedded_vm->realm(), &storage, true);
    js_array_buffer_set_max_byte_length(buffer, 64 * KiB);

    EXPECT(js_array_buffer_is_shared_array_buffer(buffer));
    EXPECT(!js_array_buffer_is_fixed_length(buffer));
    EXPECT_EQ(js_array_buffer_max_byte_length(buffer), 64u * KiB);
    EXPECT_EQ(js_array_buffer_byte_length(buffer), 16u);
    EXPECT(gc_primitive_storage_resize(handle, 32 * KiB, true, nullptr));
    EXPECT_EQ(js_array_buffer_byte_length(buffer), 32u * KiB);
}

namespace {

NEVER_INLINE void make_storage_owners_of_which_buffers_view_every_other(EmbeddedVM& embedded_vm, Vector<GCRoot*>& buffer_roots)
{
    for (size_t index = 0; index < storage_owner_count; ++index) {
        GCPrimitiveStorageHandle handle = GC_PRIMITIVE_STORAGE_NULL_HANDLE;
        VERIFY(gc_primitive_storage_allocate(16, true, &handle, nullptr));
        auto owner = GC::Heap::the().allocate<StorageOwner>(index, handle);
        if (index % 2 != 0)
            continue;
        auto storage = external_storage(*owner, handle, 16);
        auto* buffer = js_array_buffer_create_with_external_storage(embedded_vm.vm(), embedded_vm.realm(), &storage, false);
        buffer_roots.append(gc_root_create(reinterpret_cast<GCCell*>(buffer)));
    }
}

}

TEST_CASE(a_buffer_keeps_the_owner_of_its_storage_alive)
{
    s_storage_owner_was_finalized.fill(false);
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    Vector<GCRoot*> buffer_roots;
    make_storage_owners_of_which_buffers_view_every_other(*embedded_vm, buffer_roots);
    scrub_stack();
    GC::Heap::the().collect_garbage();

    size_t finalized_owners_without_a_buffer = 0;
    for (size_t index = 0; index < storage_owner_count; ++index) {
        if (index % 2 == 0)
            EXPECT(!s_storage_owner_was_finalized[index]);
        else if (s_storage_owner_was_finalized[index])
            ++finalized_owners_without_a_buffer;
    }
    EXPECT(finalized_owners_without_a_buffer >= storage_owner_count / 4);

    for (auto* root : buffer_roots)
        gc_root_destroy(root);
}

// LibGC cannot map shared memory into its cage on Windows yet.
#if !defined(AK_OS_WINDOWS)
TEST_CASE(shared_memory_round_trips_through_its_descriptor)
{
    auto embedded_vm = EmbeddedVM::create_with_realm({
        .become_process_default_heap = true,
        .shared_memory_shared_array_buffers = true,
    });
    int shared_memory = -1;
    EXPECT(gc_shared_memory_create(100, &shared_memory));
    auto* first = js_array_buffer_create_from_shared_memory(embedded_vm->vm(), embedded_vm->realm(), shared_memory, 100, 1234);
    close(shared_memory);
    EXPECT(first != nullptr);
    EXPECT(js_array_buffer_is_shared_array_buffer(first));
    EXPECT(js_array_buffer_is_fixed_length(first));
    auto first_storage = storage_of(first);
    EXPECT_EQ(first_storage.kind, JS_ARRAY_BUFFER_STORAGE_SHARED_MEMORY);
    EXPECT_EQ(first_storage.shared_memory_object_id, 1234u);
    EXPECT_EQ(gc_primitive_storage_size(first_storage.handle), 100u);

    size_t byte_length = 0;
    auto* first_bytes = js_array_buffer_data(first, &byte_length);
    EXPECT_EQ(byte_length, 100u);
    EXPECT_EQ(first_bytes[99], 0);
    first_bytes[99] = 42;

    size_t sent_byte_length = 0;
    auto sent_shared_memory = js_array_buffer_duplicate_shared_memory(first, &sent_byte_length);
    EXPECT(sent_shared_memory >= 0);
    EXPECT_EQ(sent_byte_length, 100u);
    auto* second = js_array_buffer_create_from_shared_memory(embedded_vm->vm(), embedded_vm->realm(), sent_shared_memory, sent_byte_length, first_storage.shared_memory_object_id);
    close(sent_shared_memory);
    EXPECT(second != nullptr);

    auto* second_bytes = js_array_buffer_data(second, &byte_length);
    EXPECT(second_bytes != first_bytes);
    EXPECT_EQ(second_bytes[99], 42);
    second_bytes[0] = 7;
    EXPECT_EQ(first_bytes[0], 7);
    EXPECT(js_array_buffer_shares_storage_with(first, second));
    EXPECT_EQ(storage_of(second).shared_memory_object_id, 1234u);

    int small_shared_memory = -1;
    EXPECT(gc_shared_memory_create(4096, &small_shared_memory));
    EXPECT(js_array_buffer_create_from_shared_memory(embedded_vm->vm(), embedded_vm->realm(), small_shared_memory, 1 * MiB, 1) == nullptr);
    EXPECT(js_array_buffer_create_from_shared_memory(embedded_vm->vm(), embedded_vm->realm(), small_shared_memory, 0, 1) == nullptr);
    close(small_shared_memory);

    auto* owned = js_array_buffer_create_from_bytes(embedded_vm->vm(), embedded_vm->realm(), first_bytes, 100, true);
    EXPECT_EQ(storage_of(owned).kind, JS_ARRAY_BUFFER_STORAGE_OWNED);
    EXPECT_EQ(js_array_buffer_duplicate_shared_memory(owned, &sent_byte_length), -1);
    EXPECT(!js_array_buffer_shares_storage_with(owned, first));
}
#endif

TEST_CASE(detaching_takes_the_detach_key)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* buffer = pointer_of_payload<JSObject>(js_array_buffer_create(embedded_vm->vm(), embedded_vm->realm(), 16, false));
    EXPECT(buffer != nullptr);
    auto* view = js_typed_array_create_on_buffer(embedded_vm->vm(), embedded_vm->realm(), JS_LAYOUT_TYPED_ARRAY_KIND_UINT32, 4, buffer);
    auto* data_view = js_array_buffer_create_data_view(embedded_vm->vm(), embedded_vm->realm(), buffer, { .length = 8, .kind = JS_BYTE_LENGTH_LENGTH }, 4);
    EXPECT(js_array_buffer_is_array_buffer(buffer));
    EXPECT(!js_array_buffer_is_array_buffer(view));
    EXPECT(js_typed_array_is_typed_array(view));
    EXPECT(!js_typed_array_is_typed_array(data_view));
    EXPECT(js_array_buffer_is_data_view(data_view));
    EXPECT(!js_array_buffer_is_data_view(buffer));
    EXPECT_EQ(js_array_buffer_data_view_viewed_buffer(data_view), buffer);
    EXPECT_EQ(js_array_buffer_data_view_byte_offset(data_view), 4u);
    auto data_view_record = js_array_buffer_make_data_view_witness_record(data_view, JS_ARRAY_BUFFER_ORDER_SEQ_CST);
    EXPECT(!js_array_buffer_is_data_view_out_of_bounds(&data_view_record));
    EXPECT_EQ(js_array_buffer_data_view_view_byte_length(&data_view_record), 8u);

    auto undefined = js_array_buffer_detach_key(buffer);
    auto key = bit_cast<JSValue>(42.5);
    js_array_buffer_set_detach_key(buffer, key);
    EXPECT_EQ(js_array_buffer_detach_key(buffer), key);
    EXPECT_EQ(js_array_buffer_detach(embedded_vm->vm(), buffer, undefined).variant, JS_COMPLETION_THROW);
    EXPECT(!js_array_buffer_is_detached(buffer));

    EXPECT_EQ(js_array_buffer_detach(embedded_vm->vm(), buffer, key).variant, JS_COMPLETION_NORMAL);
    EXPECT(js_array_buffer_is_detached(buffer));
    EXPECT_EQ(js_array_buffer_byte_length(buffer), 0u);
    EXPECT_EQ(storage_of(buffer).kind, JS_ARRAY_BUFFER_STORAGE_DETACHED);
    size_t byte_length = 1;
    EXPECT(js_array_buffer_data(buffer, &byte_length) == nullptr);
    EXPECT_EQ(byte_length, 0u);

    auto record = js_typed_array_make_witness_record(view, JS_ARRAY_BUFFER_ORDER_SEQ_CST);
    EXPECT_EQ(record.cached_buffer_byte_length.kind, JS_BYTE_LENGTH_DETACHED);
    EXPECT(js_typed_array_is_out_of_bounds(&record));
    EXPECT_EQ(js_typed_array_byte_length_of_witness(&record), 0u);
    data_view_record = js_array_buffer_make_data_view_witness_record(data_view, JS_ARRAY_BUFFER_ORDER_SEQ_CST);
    EXPECT(js_array_buffer_is_data_view_out_of_bounds(&data_view_record));
}

TEST_CASE(owned_storage_resizes_within_its_maximum)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* typed_array = pointer_of_payload<JSObject>(js_typed_array_create(embedded_vm->vm(), embedded_vm->realm(), JS_LAYOUT_TYPED_ARRAY_KIND_FLOAT64, 4));
    auto* buffer = js_typed_array_viewed_array_buffer(typed_array);
    EXPECT_EQ(js_array_buffer_byte_length(buffer), 32u);
    EXPECT_EQ(storage_of(buffer).kind, JS_ARRAY_BUFFER_STORAGE_OWNED);
    js_array_buffer_set_max_byte_length(buffer, 8192);

    // The runtime's own resize hook, which an embedder's hook falls back to for the buffers it does not handle.
    auto resized = js_vm_default_host_resize_array_buffer(embedded_vm->vm(), buffer, 4096);
    EXPECT_EQ(resized.variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(resized.payload, static_cast<u64>(JS_HANDLED_BY_HOST_HANDLED));
    EXPECT_EQ(js_array_buffer_byte_length(buffer), 4096u);
    size_t byte_length = 0;
    auto* data = js_array_buffer_data(buffer, &byte_length);
    EXPECT_EQ(byte_length, 4096u);
    EXPECT_EQ(data[4095], 0);
    EXPECT_EQ(js_vm_default_host_resize_array_buffer(embedded_vm->vm(), buffer, SIZE_MAX / 2).variant, JS_COMPLETION_THROW);
    EXPECT_EQ(js_array_buffer_byte_length(buffer), 4096u);
}

TEST_CASE(a_growable_alias_of_a_shared_array_buffer_grows_the_buffer_that_owns_the_storage)
{
    auto embedded_vm = EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options);
    auto* owner = pointer_of_payload<JSObject>(js_array_buffer_create(embedded_vm->vm(), embedded_vm->realm(), 8, true));
    js_array_buffer_set_max_byte_length(owner, 64 * KiB);
    auto owner_storage = storage_of(owner);
    EXPECT_EQ(owner_storage.kind, JS_ARRAY_BUFFER_STORAGE_OWNED);

    JSExternalPrimitiveStorage aliased_storage {
        .owner = owner,
        .handle = owner_storage.handle,
        .fixed_byte_length = 0,
        .has_fixed_byte_length = false,
    };
    auto* alias = js_array_buffer_create_with_external_storage(embedded_vm->vm(), embedded_vm->realm(), &aliased_storage, true);
    js_array_buffer_set_max_byte_length(alias, 64 * KiB);

    // The runtime's own resize hook takes the storage step that SharedArrayBuffer.prototype.grow() takes too.
    EXPECT_EQ(js_vm_default_host_resize_array_buffer(embedded_vm->vm(), alias, 32 * KiB).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_array_buffer_byte_length(owner), 32u * KiB);
    EXPECT_EQ(js_array_buffer_byte_length(alias), 32u * KiB);
    size_t owner_byte_length = 0;
    size_t alias_byte_length = 0;
    EXPECT_EQ(js_array_buffer_data(alias, &alias_byte_length), js_array_buffer_data(owner, &owner_byte_length));
    EXPECT(js_array_buffer_shares_storage_with(alias, owner));
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/ByteBuffer.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <LibCrypto/BigInt/SignedBigInteger.h>
#include <LibCrypto/BigInt/UnsignedBigInteger.h>
#include <LibGC/Cell.h>
#include <LibGC/CellAllocator.h>
#include <LibGC/Root.h>
#include <LibGC/Weak.h>
#include <LibGC/WeakInlines.h>
#include <LibJS/Runtime/ArrayBuffer.h>
#include <LibJS/Runtime/BigInt.h>
#include <LibJS/Runtime/ByteLength.h>
#include <LibJS/Runtime/Completion.h>
#include <LibJS/Runtime/DataView.h>
#include <LibJS/Runtime/ErrorTypes.h>
#include <LibJS/Runtime/ExecutionContext.h>
#include <LibJS/Runtime/Intrinsics.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/SharedArrayBufferConstructor.h>
#include <LibJS/Runtime/TypedArray.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/ValueInlines.h>
#include <LibJS/Script.h>
#include <LibTest/TestCase.h>

// ArrayBuffers, SharedArrayBuffers, typed arrays and DataViews as LibJS's users create and read them. The same
// expectations hold for the C++ runtime's LibJS and for the facade over the Rust one.

using namespace JS;

namespace {

struct VMWithRealm {
    VMWithRealm()
        : vm(VM::create())
        , realm_execution_context(MUST(Realm::initialize_host_defined_realm(*vm, nullptr, nullptr)))
    {
    }

    ~VMWithRealm()
    {
        while (!vm->execution_context_stack().is_empty())
            vm->pop_execution_context();
    }

    Realm& realm() { return *realm_execution_context->realm; }

    NonnullRefPtr<VM> vm;
    NonnullOwnPtr<ExecutionContext> realm_execution_context;
};

// One of the embedder's cells that owns storage of LibGC's primitive storage, which it lends to ArrayBuffers, as an
// AudioBuffer does with its channels.
class StorageOwner final : public GC::Cell {
    GC_CELL(StorageOwner, GC::Cell);
    GC_DECLARE_ALLOCATOR(StorageOwner);

public:
    explicit StorageOwner(size_t size)
        : storage(MUST(DataBlock::OwnedBackingStore::create_zeroed(size)))
    {
    }

    DataBlock::OwnedBackingStore storage;
};

GC_DEFINE_ALLOCATOR(StorageOwner);

}

static ThrowCompletionOr<Value> evaluate(VM& vm, Realm& realm, StringView source)
{
    auto source_text = Utf16String::from_utf8(source);
    auto script = Script::parse(source_text.utf16_view(), realm);
    VERIFY(!script.is_error());
    return vm.run(script.value());
}

template<typename T>
static T& object_of(VM& vm, Realm& realm, StringView source)
{
    return as<T>(MUST(evaluate(vm, realm, source)).as_object());
}

static Value property_of(VM& vm, Value value, StringView name)
{
    return MUST(value.get(vm, PropertyKey { Utf16FlyString::from_utf8(name) }));
}

static Value element_of(VM& vm, Value value, u32 index)
{
    return MUST(value.get(vm, PropertyKey { index }));
}

static Utf16String name_of_thrown_error(VM& vm, Completion const& completion)
{
    VERIFY(completion.is_error());
    return MUST(property_of(vm, completion.value(), "name"sv).to_utf16_string(vm));
}

static bool names_the_same_storage(GC::PrimitiveStorageHandle handle, GC::PrimitiveStorageHandle other_handle)
{
    return handle.index == other_handle.index && handle.generation == other_handle.generation;
}

static ByteBuffer bytes_of(ArrayBuffer const& buffer)
{
    return MUST(buffer.copy_to_byte_buffer());
}

static ByteBuffer bytes(std::initializer_list<u8> values)
{
    return MUST(ByteBuffer::copy(values.begin(), values.size()));
}

// Collects garbage once the stack below the caller no longer holds pointers left over from earlier calls, which the
// conservative scan would treat as roots.
static NEVER_INLINE void collect_garbage(VM& vm)
{
    u8 volatile filler[8 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
    vm.heap().collect_garbage();
}

TEST_CASE(array_buffers)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto zeroed = MUST(ArrayBuffer::create(realm, 8));
    EXPECT_EQ(zeroed->byte_length(), 8u);
    EXPECT(!zeroed->is_detached());
    EXPECT(zeroed->is_fixed_length());
    EXPECT(!zeroed->is_shared_array_buffer());
    EXPECT_EQ(bytes_of(zeroed), bytes({ 0, 0, 0, 0, 0, 0, 0, 0 }));
    EXPECT(same_value(property_of(vm, zeroed, "byteLength"sv), Value(8)));
    EXPECT(same_value(MUST(Value(zeroed).get(vm, vm.names.constructor)), MUST(evaluate(vm, realm, "ArrayBuffer"sv))));
    EXPECT(zeroed->detach_key().is_undefined());

    // What the buffer's users write and read reaches the bytes that its script views see.
    u8 const written[] = { 1, 2, 3, 4 };
    zeroed->overwrite(2, written, sizeof(written));
    EXPECT_EQ(bytes_of(zeroed), bytes({ 0, 0, 1, 2, 3, 4, 0, 0 }));
    EXPECT_EQ(*zeroed->data_at(3), 2);
    zeroed->data_at(7)[0] = 9;
    EXPECT_EQ(MUST(zeroed->copy_to_byte_buffer(5, 3)), bytes({ 4, 0, 9 }));
    u8 copied[2] {};
    zeroed->copy_to(4, { copied, sizeof(copied) });
    EXPECT_EQ(copied[0], 3);
    EXPECT_EQ(copied[1], 4);
    zeroed->with_readonly_bytes(2, 3, [&](ReadonlyBytes readonly_bytes) {
        EXPECT_EQ(readonly_bytes.size(), 3u);
        EXPECT_EQ(readonly_bytes.data(), zeroed->data_at(2));
        EXPECT_EQ(readonly_bytes[2], 3);
    });
    auto view_of_zeroed = Uint8Array::create(realm, 8, zeroed);
    EXPECT(same_value(element_of(vm, view_of_zeroed, 7), Value(9)));

    auto copy = ArrayBuffer::create(realm, bytes({ 5, 6, 7 }));
    EXPECT_EQ(copy->byte_length(), 3u);
    EXPECT_EQ(bytes_of(copy), bytes({ 5, 6, 7 }));
    EXPECT(!copy->shares_storage_with(zeroed));
    EXPECT(copy->shares_storage_with(copy));

    zeroed->copy_data_to(copy, 2, 1, 2);
    EXPECT_EQ(bytes_of(copy), bytes({ 5, 1, 2 }));

    auto empty = ArrayBuffer::create(realm, ByteBuffer {});
    EXPECT_EQ(empty->byte_length(), 0u);
    EXPECT_EQ(bytes_of(empty), ByteBuffer {});
    empty->with_readonly_bytes(0, 0, [](ReadonlyBytes readonly_bytes) { EXPECT(readonly_bytes.is_empty()); });

    // GetValueFromBuffer of each kind of element, in either byte order.
    auto values = ArrayBuffer::create(realm, bytes({ 0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0xc0, 0x3f }));
    EXPECT(same_value(values->get_value<u8>(0, true, ArrayBuffer::Unordered), Value(0xfe)));
    EXPECT(same_value(values->get_value<i8>(0, true, ArrayBuffer::Unordered), Value(-2)));
    EXPECT(same_value(values->get_value<u16>(0, true, ArrayBuffer::Unordered), Value(0xfffe)));
    EXPECT(same_value(values->get_value<u16>(0, false, ArrayBuffer::Unordered, false), Value(0xfeff)));
    EXPECT(same_value(values->get_value<i32>(0, true, ArrayBuffer::SeqCst), Value(-2)));
    EXPECT(same_value(values->get_value<u32>(0, true, ArrayBuffer::SeqCst), Value(0xfffffffeu)));
    EXPECT(same_value(values->get_value<float>(8, true, ArrayBuffer::Unordered), Value(1.5)));
    EXPECT(same_value(values->get_value<u8>(10, false, ArrayBuffer::Unordered), Value(0xc0)));
    EXPECT_EQ(values->get_value<i64>(0, true, ArrayBuffer::Unordered).as_bigint().big_integer(), Crypto::SignedBigInteger { -2 });
    EXPECT_EQ(values->get_value<u64>(0, true, ArrayBuffer::Unordered).as_bigint().big_integer(), Crypto::SignedBigInteger { Crypto::UnsignedBigInteger { 0xfffffffffffffffeull } });

    // A buffer that script creates is one of LibJS's ArrayBuffers too.
    auto& resizable = object_of<ArrayBuffer>(vm, realm, "globalThis.resizable = new ArrayBuffer(4, { maxByteLength: 16 })"sv);
    EXPECT(!resizable.is_fixed_length());
    EXPECT_EQ(resizable.max_byte_length(), 16u);
    MUST(resizable.try_resize(12, DataBlock::ZeroFillNewBytes::Yes));
    EXPECT_EQ(resizable.byte_length(), 12u);
    EXPECT(same_value(MUST(evaluate(vm, realm, "resizable.byteLength"sv)), Value(12)));
    MUST(resizable.try_resize(2));
    EXPECT(same_value(MUST(evaluate(vm, realm, "resizable.byteLength"sv)), Value(2)));

    zeroed->set_max_byte_length(32);
    EXPECT(!zeroed->is_fixed_length());
    EXPECT_EQ(zeroed->max_byte_length(), 32u);
    MUST(zeroed->try_resize(4));
    EXPECT_EQ(bytes_of(zeroed), bytes({ 0, 0, 1, 2 }));

    EXPECT(is<ArrayBuffer>(MUST(evaluate(vm, realm, "new SharedArrayBuffer(4)"sv)).as_object()));
    EXPECT(is<ArrayBuffer>(MUST(evaluate(vm, realm, "new (class extends ArrayBuffer {})(4)"sv)).as_object()));
    EXPECT(!is<ArrayBuffer>(MUST(evaluate(vm, realm, "new DataView(new ArrayBuffer(4))"sv)).as_object()));
    EXPECT(!is<ArrayBuffer>(MUST(evaluate(vm, realm, "new Uint8Array(4)"sv)).as_object()));
    EXPECT(!is<ArrayBuffer>(realm.global_object()));
}

TEST_CASE(detaching_array_buffers)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto& buffer = object_of<ArrayBuffer>(vm, realm, "globalThis.buffer = new ArrayBuffer(8); globalThis.view = new Uint8Array(buffer, 2); buffer"sv);
    auto& view = object_of<Uint8Array>(vm, realm, "view"sv);
    MUST(detach_array_buffer(vm, buffer));
    EXPECT(buffer.is_detached());
    EXPECT_EQ(buffer.byte_length(), 0u);
    EXPECT(MUST(evaluate(vm, realm, "buffer.detached"sv)).as_bool());
    EXPECT(same_value(MUST(evaluate(vm, realm, "view.length"sv)), Value(0)));
    EXPECT(is_typed_array_out_of_bounds(make_typed_array_with_buffer_witness_record(view, ArrayBuffer::Order::SeqCst)));

    // Only the key a buffer was given detaches it.
    auto keyed = MUST(ArrayBuffer::create(realm, 4));
    auto key = PrimitiveString::create(vm, "key"_utf16);
    keyed->set_detach_key(key);
    EXPECT(same_value(keyed->detach_key(), key));
    auto mismatch = detach_array_buffer(vm, keyed);
    EXPECT(mismatch.is_throw_completion());
    EXPECT_EQ(name_of_thrown_error(vm, mismatch.throw_completion()), "TypeError"sv);
    EXPECT(!keyed->is_detached());
    EXPECT(detach_array_buffer(vm, keyed, Value(1)).is_throw_completion());
    MUST(detach_array_buffer(vm, keyed, key));
    EXPECT(keyed->is_detached());

    // Taking the data block out of a buffer detaches it, and the block's bytes move to the buffer it is given to.
    auto source = ArrayBuffer::create(realm, bytes({ 1, 2, 3, 4 }));
    auto view_of_source = Uint8Array::create(realm, 4, source);
    auto block = MUST(source->detach_and_take_data_block(vm));
    EXPECT(source->is_detached());
    EXPECT_EQ(block.size(), 4u);
    EXPECT_EQ(MUST(block.copy_to_byte_buffer()), bytes({ 1, 2, 3, 4 }));
    EXPECT(is_typed_array_out_of_bounds(make_typed_array_with_buffer_witness_record(view_of_source, ArrayBuffer::Order::SeqCst)));
    auto transferred = ArrayBuffer::create(realm, move(block));
    EXPECT(!transferred->is_detached());
    EXPECT(!transferred->is_shared_array_buffer());
    EXPECT_EQ(bytes_of(transferred), bytes({ 1, 2, 3, 4 }));
    collect_garbage(vm);
    EXPECT_EQ(bytes_of(transferred), bytes({ 1, 2, 3, 4 }));

    // A block that nobody takes over frees its bytes.
    {
        auto dropped = ArrayBuffer::create(realm, bytes({ 5, 6 }));
        auto dropped_block = MUST(dropped->detach_and_take_data_block(vm));
        EXPECT(dropped->is_detached());
        EXPECT_EQ(dropped_block.size(), 2u);
    }

    // A buffer with a detach key keeps its data block.
    auto kept = ArrayBuffer::create(realm, bytes({ 7 }));
    kept->set_detach_key(key);
    auto refused = kept->detach_and_take_data_block(vm);
    EXPECT(refused.is_throw_completion());
    EXPECT_EQ(name_of_thrown_error(vm, refused.throw_completion()), "TypeError"sv);
    EXPECT(!kept->is_detached());
    EXPECT_EQ(bytes_of(kept), bytes({ 7 }));

    // CreateByteDataBlock and CopyDataBlockBytes, as StructuredSerialize copies a buffer with them.
    auto data_copy = MUST(create_byte_data_block(vm, 3));
    EXPECT_EQ(MUST(data_copy.copy_to_byte_buffer()), bytes({ 0, 0, 0 }));
    transferred->copy_data_to(data_copy, 1, 0, 3);
    EXPECT_EQ(MUST(data_copy.copy_to_byte_buffer()), bytes({ 2, 3, 4 }));
    auto too_large = create_byte_data_block(vm, 1ull << 60);
    EXPECT(too_large.is_throw_completion());
    EXPECT_EQ(name_of_thrown_error(vm, too_large.throw_completion()), "RangeError"sv);

    // CloneArrayBuffer copies the bytes into a new buffer of the current realm.
    auto* clone = MUST(clone_array_buffer(vm, transferred, 1, 2));
    EXPECT_EQ(bytes_of(*clone), bytes({ 2, 3 }));
    EXPECT(!clone->shares_storage_with(transferred));
    EXPECT(same_value(MUST(Value(clone).get(vm, vm.names.constructor)), MUST(evaluate(vm, realm, "ArrayBuffer"sv))));
}

TEST_CASE(external_storage)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    GC::Ptr<StorageOwner> owner = vm.heap().allocate<StorageOwner>(8);
    owner->storage.data()[1] = 42;

    auto buffer = ArrayBuffer::create(realm, DataBlock { DataBlock::ExternalPrimitiveStorage { GC::Ref<GC::Cell> { *owner }, owner->storage.handle(), 4 }, DataBlock::Shared::No });
    EXPECT_EQ(buffer->byte_length(), 4u);
    EXPECT(buffer->is_fixed_length());
    EXPECT_EQ(bytes_of(buffer), bytes({ 0, 42, 0, 0 }));
    EXPECT_EQ(buffer->data_at(0), owner->storage.data());

    buffer->overwrite(3, "\x07", 1);
    EXPECT_EQ(owner->storage.data()[3], 7);

    auto const& block = buffer->data_block();
    auto const& external = block.byte_buffer.get<DataBlock::ExternalPrimitiveStorage>();
    EXPECT_EQ(external.owner.ptr(), static_cast<GC::Cell*>(owner.ptr()));
    EXPECT(names_the_same_storage(external.handle, owner->storage.handle()));
    EXPECT_EQ(external.byte_length(), 4u);

    // The buffer keeps the cell that owns its storage alive.
    GC::Weak<StorageOwner> weak_owner { *owner };
    owner = nullptr;
    collect_garbage(vm);
    EXPECT(weak_owner);
    EXPECT_EQ(bytes_of(buffer), bytes({ 0, 42, 0, 7 }));

    // A buffer without a fixed byte length has the size of the storage, and refreshing it switches its storage.
    auto second_owner = vm.heap().allocate<StorageOwner>(6);
    buffer->set_data_block(DataBlock { DataBlock::ExternalPrimitiveStorage { GC::Ref<GC::Cell> { *second_owner }, second_owner->storage.handle() }, DataBlock::Shared::No });
    EXPECT_EQ(buffer->byte_length(), 6u);
    EXPECT_EQ(buffer->data_at(0), second_owner->storage.data());

    auto view = Uint16Array::create(realm, 3, buffer);
    second_owner->storage.data()[4] = 1;
    second_owner->storage.data()[5] = 1;
    EXPECT(same_value(element_of(vm, view, 2), Value(0x101)));

    // A SharedArrayBuffer over storage that a cell owns.
    auto shared = ArrayBuffer::create(realm, DataBlock { DataBlock::ExternalPrimitiveStorage { GC::Ref<GC::Cell> { *second_owner }, second_owner->storage.handle(), 6 }, DataBlock::Shared::Yes });
    EXPECT(shared->is_shared_array_buffer());
    EXPECT(shared->shares_storage_with(buffer));
    EXPECT(same_value(MUST(Value(shared).get(vm, vm.names.constructor)), MUST(evaluate(vm, realm, "SharedArrayBuffer"sv))));
}

TEST_CASE(shared_array_buffers)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    // A fixed-length SharedArrayBuffer lives in shared memory, which another agent can map.
    auto source = MUST(allocate_shared_array_buffer(vm, *realm.intrinsics().shared_array_buffer_constructor(), 8));
    EXPECT(source->is_shared_array_buffer());
    EXPECT(source->is_fixed_length());
    EXPECT_EQ(source->byte_length(), 8u);
    EXPECT(same_value(MUST(Value(source).get(vm, vm.names.constructor)), MUST(evaluate(vm, realm, "SharedArrayBuffer"sv))));
    auto shared_memory = source->shared_buffer();
    EXPECT(shared_memory.has_value());
    EXPECT_EQ(shared_memory->size(), 8u);
    EXPECT_NE(source->shared_object_id(), 0u);

    auto const& block = source->data_block();
    EXPECT(block.is_shared == DataBlock::Shared::Yes);
    EXPECT_EQ(block.byte_buffer.get<DataBlock::SharedBackingStore>().object_id, source->shared_object_id());
    EXPECT_EQ(block.shared_object_id(), source->shared_object_id());

    // A buffer over the same shared memory object shares its bytes.
    auto mapped = ArrayBuffer::create(realm, shared_memory.release_value(), source->shared_object_id());
    EXPECT(mapped->is_shared_array_buffer());
    EXPECT_EQ(mapped->shared_object_id(), source->shared_object_id());
    EXPECT(mapped->shares_storage_with(source));
    EXPECT(source->shares_storage_with(mapped));
    source->overwrite(0, "S", 1);
    EXPECT_EQ(bytes_of(mapped)[0], static_cast<u8>('S'));
    mapped->overwrite(1, "M", 1);
    EXPECT_EQ(bytes_of(source)[1], static_cast<u8>('M'));

    // The bytes of a SharedArrayBuffer may change at any time, so they are read through a snapshot.
    source->with_readonly_bytes(0, 2, [&](ReadonlyBytes readonly_bytes) {
        EXPECT_NE(readonly_bytes.data(), source->data_at(0));
        EXPECT_EQ(readonly_bytes[0], static_cast<u8>('S'));
        EXPECT_EQ(readonly_bytes[1], static_cast<u8>('M'));
    });
    EXPECT(same_value(source->get_value<u8>(1, true, ArrayBuffer::SeqCst), Value(static_cast<i32>('M'))));
    EXPECT(same_value(source->get_value<u16>(0, true, ArrayBuffer::Unordered), Value('S' | ('M' << 8))));

    // A process-local SharedArrayBuffer is shared by aliasing the storage of the buffer that owns it, as
    // StructuredSerialize does.
    auto local = MUST(ArrayBuffer::create(realm, 4, DataBlock::Shared::Yes));
    EXPECT(local->is_shared_array_buffer());
    EXPECT(!local->shared_buffer().has_value());
    EXPECT_EQ(local->shared_object_id(), 0u);
    auto const& local_block = local->data_block();
    auto const& owned = local_block.byte_buffer.get<DataBlock::OwnedBackingStore>();
    auto alias = ArrayBuffer::create(realm, DataBlock { DataBlock::ExternalPrimitiveStorage { GC::Ref<GC::Cell> { local }, owned.handle(), 4 }, DataBlock::Shared::Yes });
    EXPECT(alias->shares_storage_with(local));
    EXPECT(alias->data_block().byte_buffer.get<DataBlock::ExternalPrimitiveStorage>().owner == local);
    local->overwrite(3, "L", 1);
    EXPECT_EQ(bytes_of(alias)[3], static_cast<u8>('L'));
    EXPECT(!alias->shares_storage_with(source));

    // A growable one reserves its maximum byte length.
    auto growable = MUST(allocate_shared_array_buffer(vm, *realm.intrinsics().shared_array_buffer_constructor(), 2, 16));
    EXPECT(growable->is_shared_array_buffer());
    EXPECT(!growable->is_fixed_length());
    EXPECT_EQ(growable->byte_length(), 2u);
    EXPECT_EQ(growable->max_byte_length(), 16u);
    auto too_long = allocate_shared_array_buffer(vm, *realm.intrinsics().shared_array_buffer_constructor(), 32, 16);
    EXPECT(too_long.is_throw_completion());
    EXPECT_EQ(name_of_thrown_error(vm, too_long.throw_completion()), "RangeError"sv);

    // A zero-length SharedArrayBuffer has no shared memory object.
    auto empty = MUST(allocate_shared_array_buffer(vm, *realm.intrinsics().shared_array_buffer_constructor(), 0));
    EXPECT(empty->is_shared_array_buffer());
    EXPECT_EQ(empty->byte_length(), 0u);
}

TEST_CASE(typed_arrays)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

#define __JS_ENUMERATE(ClassName, snake_name, PrototypeName, ConstructorName, Type)                                            \
    {                                                                                                                          \
        auto typed_array = MUST(ClassName::create(realm, 3));                                                                  \
        Object& object = *typed_array;                                                                                         \
        EXPECT(is<ClassName>(object));                                                                                         \
        EXPECT(is<TypedArrayBase>(object));                                                                                    \
        EXPECT(typed_array->kind() == TypedArrayBase::Kind::ClassName);                                                        \
        EXPECT_EQ(typed_array->element_size(), typed_array_element_size(TypedArrayBase::Kind::ClassName));                     \
        EXPECT_EQ(typed_array->element_size(), sizeof(Conditional<IsSame<ClampedU8, Type>, u8, Type>));                        \
        EXPECT_EQ(typed_array_element_name(typed_array->kind()), #ClassName##sv);                                              \
        EXPECT_EQ(typed_array->array_length().length(), 3u);                                                                   \
        EXPECT_EQ(typed_array->byte_length().length(), 3 * typed_array->element_size());                                       \
        EXPECT_EQ(typed_array->byte_offset(), 0u);                                                                             \
        EXPECT_EQ(typed_array->viewed_array_buffer()->byte_length(), 3 * typed_array->element_size());                         \
        EXPECT(same_value(property_of(vm, typed_array, "BYTES_PER_ELEMENT"sv), Value(typed_array->element_size())));           \
        EXPECT(same_value(MUST(Value(typed_array).get(vm, vm.names.constructor)), MUST(evaluate(vm, realm, #ClassName##sv)))); \
        auto& from_script = object_of<TypedArrayBase>(vm, realm, "new " #ClassName "(2)"sv);                                   \
        EXPECT(is<ClassName>(from_script));                                                                                    \
        EXPECT(from_script.kind() == TypedArrayBase::Kind::ClassName);                                                         \
    }
    JS_ENUMERATE_TYPED_ARRAYS
#undef __JS_ENUMERATE

    EXPECT(!is<Uint8Array>(object_of<Object>(vm, realm, "new Int8Array(1)"sv)));
    EXPECT(!is<TypedArrayBase>(object_of<Object>(vm, realm, "new DataView(new ArrayBuffer(1))"sv)));
    EXPECT(!is<TypedArrayBase>(object_of<Object>(vm, realm, "[1, 2]"sv)));
    EXPECT(!is<TypedArrayBase>(realm.global_object()));

    // A view of part of a buffer that script created.
    auto& int32_array = object_of<Int32Array>(vm, realm, "globalThis.buffer = new ArrayBuffer(16); globalThis.int32_array = new Int32Array(buffer, 4, 2)"sv);
    EXPECT_EQ(int32_array.byte_offset(), 4u);
    EXPECT_EQ(int32_array.array_length().length(), 2u);
    EXPECT_EQ(int32_array.byte_length().length(), 8u);
    EXPECT_EQ(int32_array.viewed_array_buffer(), &object_of<ArrayBuffer>(vm, realm, "buffer"sv));
    int32_array.viewed_array_buffer()->overwrite(8, "\xff\xff\xff\xff", 4);
    EXPECT(same_value(MUST(evaluate(vm, realm, "int32_array[1]"sv)), Value(-1)));
    MUST(evaluate(vm, realm, "int32_array[0] = 0x01020304"sv));
    EXPECT_EQ(MUST(int32_array.viewed_array_buffer()->copy_to_byte_buffer(4, 4)), bytes({ 4, 3, 2, 1 }));

    auto witness = make_typed_array_with_buffer_witness_record(int32_array, ArrayBuffer::Order::SeqCst);
    EXPECT_EQ(witness.object.ptr(), &int32_array);
    EXPECT_EQ(witness.cached_buffer_byte_length.length(), 16u);
    EXPECT(!is_typed_array_out_of_bounds(witness));
    EXPECT_EQ(typed_array_length(witness), 2u);
    EXPECT_EQ(typed_array_byte_length(witness), 8u);

    // A view of a buffer that the embedder created.
    auto buffer = ArrayBuffer::create(realm, bytes({ 0, 0, 0, 0, 0, 0, 0xc0, 0x3f }));
    auto float32_array = Float32Array::create(realm, 2, buffer);
    EXPECT_EQ(float32_array->viewed_array_buffer(), buffer.ptr());
    EXPECT(same_value(element_of(vm, float32_array, 1), Value(1.5)));
    EXPECT(same_value(element_of(vm, float32_array, 2), js_undefined()));

    // A view restored from its slots, as StructuredDeserialize restores one.
    auto restored = TypedArrayBase::create_from_slots(realm, TypedArrayBase::Kind::Uint16Array, buffer, ByteLength { 2 }, ByteLength { 4 }, 4);
    EXPECT(is<Uint16Array>(*restored));
    EXPECT(!is<Uint8Array>(*restored));
    EXPECT_EQ(restored->byte_offset(), 4u);
    EXPECT_EQ(restored->array_length().length(), 2u);
    EXPECT_EQ(restored->byte_length().length(), 4u);
    EXPECT(same_value(element_of(vm, restored, 1), Value(0x3fc0)));

    // A view that tracks the length of a resizable buffer.
    auto resizable = MUST(ArrayBuffer::create(realm, 8));
    resizable->set_max_byte_length(16);
    auto tracking = TypedArrayBase::create_from_slots(realm, TypedArrayBase::Kind::Uint16Array, resizable, ByteLength::auto_(), ByteLength::auto_(), 2);
    EXPECT(tracking->array_length().is_auto());
    EXPECT(tracking->byte_length().is_auto());
    EXPECT_EQ(typed_array_length(make_typed_array_with_buffer_witness_record(tracking, ArrayBuffer::Order::SeqCst)), 3u);
    MUST(resizable->try_resize(12, DataBlock::ZeroFillNewBytes::Yes));
    EXPECT_EQ(typed_array_length(make_typed_array_with_buffer_witness_record(tracking, ArrayBuffer::Order::SeqCst)), 5u);
    MUST(resizable->try_resize(1));
    auto out_of_bounds = make_typed_array_with_buffer_witness_record(tracking, ArrayBuffer::Order::SeqCst);
    EXPECT(is_typed_array_out_of_bounds(out_of_bounds));
    EXPECT_EQ(typed_array_byte_length(out_of_bounds), 0u);

    auto& fixed_view_of_resizable = object_of<Uint8Array>(vm, realm, "new Uint8Array(new ArrayBuffer(4, { maxByteLength: 8 }), 0, 2)"sv);
    EXPECT(!fixed_view_of_resizable.array_length().is_auto());
    EXPECT_EQ(fixed_view_of_resizable.array_length().length(), 2u);

    // TypedArrayFrom accepts typed arrays only.
    EXPECT_EQ(MUST(typed_array_from(vm, float32_array)), float32_array.ptr());
    auto not_a_typed_array = typed_array_from(vm, buffer);
    EXPECT(not_a_typed_array.is_throw_completion());
    EXPECT_EQ(name_of_thrown_error(vm, not_a_typed_array.throw_completion()), "TypeError"sv);
    EXPECT(typed_array_from(vm, Value(1)).is_throw_completion());
    auto undefined = typed_array_from(vm, js_undefined());
    EXPECT(undefined.is_throw_completion());
    EXPECT_EQ(name_of_thrown_error(vm, undefined.throw_completion()), "TypeError"sv);
}

TEST_CASE(data_views)
{
    VMWithRealm vm_with_realm;
    auto& vm = *vm_with_realm.vm;
    auto& realm = vm_with_realm.realm();

    auto& data_view_object = object_of<Object>(vm, realm, "globalThis.data_view = new DataView(new ArrayBuffer(8), 2, 4)"sv);
    EXPECT(is<DataView>(data_view_object));
    EXPECT(!is<TypedArrayBase>(data_view_object));
    auto& data_view = as<DataView>(data_view_object);
    EXPECT_EQ(data_view.byte_offset(), 2u);
    EXPECT_EQ(data_view.byte_length().length(), 4u);
    EXPECT_EQ(data_view.viewed_array_buffer()->byte_length(), 8u);
    EXPECT(!is<DataView>(object_of<Object>(vm, realm, "new Uint8Array(4)"sv)));

    MUST(evaluate(vm, realm, "data_view.setUint16(0, 0x1234)"sv));
    auto& buffer = *data_view.viewed_array_buffer();
    EXPECT(same_value(buffer.get_value<u16>(2, false, ArrayBuffer::Unordered, false), Value(0x1234)));
    EXPECT(same_value(buffer.get_value<u16>(2, false, ArrayBuffer::Unordered), Value(0x3412)));
    buffer.overwrite(4, "\x01\x02", 2);
    EXPECT(same_value(MUST(evaluate(vm, realm, "data_view.getUint16(2)"sv)), Value(0x0102)));

    auto witness = make_data_view_with_buffer_witness_record(data_view, ArrayBuffer::Order::SeqCst);
    EXPECT_EQ(witness.object.ptr(), &data_view);
    EXPECT_EQ(witness.cached_buffer_byte_length.length(), 8u);
    EXPECT(!is_view_out_of_bounds(witness));
    EXPECT_EQ(get_view_byte_length(witness), 4u);

    // A view that the embedder creates, as StructuredDeserialize restores one.
    auto created = DataView::create(realm, &buffer, ByteLength { 3 }, 5);
    EXPECT(is<DataView>(static_cast<Object&>(*created)));
    EXPECT_EQ(created->viewed_array_buffer(), &buffer);
    EXPECT_EQ(created->byte_offset(), 5u);
    EXPECT_EQ(created->byte_length().length(), 3u);
    EXPECT(same_value(property_of(vm, created, "byteLength"sv), Value(3)));
    EXPECT(same_value(MUST(Value(created).get(vm, vm.names.constructor)), MUST(evaluate(vm, realm, "DataView"sv))));

    // A view that tracks the length of a resizable buffer goes out of bounds when the buffer shrinks past it.
    auto resizable = MUST(ArrayBuffer::create(realm, 8));
    resizable->set_max_byte_length(8);
    auto tracking = DataView::create(realm, resizable.ptr(), ByteLength::auto_(), 4);
    EXPECT(tracking->byte_length().is_auto());
    EXPECT_EQ(get_view_byte_length(make_data_view_with_buffer_witness_record(tracking, ArrayBuffer::Order::SeqCst)), 4u);
    MUST(resizable->try_resize(2));
    auto out_of_bounds = make_data_view_with_buffer_witness_record(tracking, ArrayBuffer::Order::SeqCst);
    EXPECT_EQ(out_of_bounds.cached_buffer_byte_length.length(), 2u);
    EXPECT(is_view_out_of_bounds(out_of_bounds));

    MUST(detach_array_buffer(vm, buffer));
    auto detached = make_data_view_with_buffer_witness_record(data_view, ArrayBuffer::Order::SeqCst);
    EXPECT(detached.cached_buffer_byte_length.is_detached());
    EXPECT(is_view_out_of_bounds(detached));
}

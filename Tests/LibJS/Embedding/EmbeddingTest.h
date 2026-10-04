/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/ByteString.h>
#include <AK/Noncopyable.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/StringView.h>
#include <AK/Utf16View.h>
#include <LibGC/NanBoxedValue.h>
#include <LibJS/Embedding/ABI.h>
#include <LibJS/Embedding/Layout.h>
#include <LibJS/HostObjectABI.h>
#include <LibTest/TestCase.h>

// A JSUtf16View of ASCII text, in the ASCII storage kind of AK::Utf16View, which holds nothing else.
inline JSUtf16View ascii_view(StringView ascii)
{
    VERIFY(ascii.is_ascii());
    return { ascii.characters_without_null_termination(), ascii.length(), true };
}

// A JSUtf16View of the code units that `view` views, in either storage kind.
inline JSUtf16View abi_view_of(Utf16View const& view)
{
    if (view.has_ascii_storage())
        return { view.ascii_span().data(), view.length_in_code_units(), true };
    return { view.utf16_span().data(), view.length_in_code_units(), false };
}

// The code units that a JSUtf16View views, which stay where they are.
inline Utf16View utf16_view_of(JSUtf16View view)
{
    if (view.length_in_code_units == 0)
        return {};
    if (view.has_ascii_storage)
        return Utf16View { StringView { static_cast<char const*>(view.data), view.length_in_code_units } };
    return Utf16View { static_cast<char16_t const*>(view.data), view.length_in_code_units };
}

inline ByteString byte_string_of(JSUtf16View view)
{
    return MUST(utf16_view_of(view).to_byte_string());
}

// The pointer that the payload of a normal completion carries, such as the cell that an operation creates.
template<typename T>
T* pointer_of_payload(JSCompletion completion)
{
    VERIFY(completion.variant == JS_COMPLETION_NORMAL);
    return reinterpret_cast<T*>(static_cast<uintptr_t>(completion.payload));
}

// The tags of JSValues, as JS::Value encodes them in both runtimes. A value that holds an object has the object's
// offset in the heap region below its tag, and one that holds an int32 has the int32.
constexpr u64 undefined_value_tag = 0b110 | GC::BASE_TAG;
constexpr u64 boolean_value_tag = 0b001 | GC::BASE_TAG;
constexpr u64 int32_value_tag = 0b010 | GC::BASE_TAG;
constexpr u64 object_value_tag = 0b001 | GC::IS_CELL_BIT;

constexpr JSValue js_undefined = undefined_value_tag << GC::TAG_SHIFT;
constexpr JSValue js_true = (boolean_value_tag << GC::TAG_SHIFT) | 1;

constexpr JSValue int32_value(i32 value)
{
    return (int32_value_tag << GC::TAG_SHIFT) | static_cast<u32>(value);
}

inline JSValue value_of_object(JSObject* object)
{
    return (object_value_tag << GC::TAG_SHIFT) | GC::NanBoxedValue::encode_pointer_bits(object);
}

// The object that `value` holds, or null if it holds something else.
inline JSObject* object_of_value(JSValue value)
{
    if ((value >> GC::TAG_SHIFT) != object_value_tag)
        return nullptr;
    return reinterpret_cast<JSObject*>(GC::NanBoxedValue::extract_pointer_bits(value));
}

// A VM constructed in storage that the test owns, the way the C++ JS::VM holds the runtime's VM in its first bytes. It
// lives on the C++ heap, as the C++ JS::VM does, so that the conservative scan of the stack never sees its fields.
class EmbeddedVM {
    AK_ALLOC_WITH_KMALLOC;
    AK_MAKE_NONCOPYABLE(EmbeddedVM);
    AK_MAKE_NONMOVABLE(EmbeddedVM);

public:
    // The setup of a VM whose heap is the one GC::Heap::the() returns, so that a test can allocate C++ GC cells in it.
    static constexpr JSVmOptions process_default_heap_options {
        .become_process_default_heap = true,
        .shared_memory_shared_array_buffers = false,
    };

    static NonnullOwnPtr<EmbeddedVM> create(JSVmOptions options = {})
    {
        return adopt_own(*new EmbeddedVM(options));
    }

    static NonnullOwnPtr<EmbeddedVM> create_with_realm(JSVmOptions options = {})
    {
        auto embedded_vm = create(options);
        embedded_vm->initialize_realm();
        return embedded_vm;
    }

    ~EmbeddedVM()
    {
        js_vm_destroy_at(vm());
    }

    JSVM* vm() { return reinterpret_cast<JSVM*>(m_storage); }

    // Creates a realm with an ordinary global object. Its execution context stays the running one until the VM is
    // destroyed, before the storage of the context is, like the root execution context of the js tool.
    void initialize_realm()
    {
        VERIFY(initialize_realm_with_global_this_value(nullptr, nullptr).variant == JS_COMPLETION_NORMAL);
    }

    // Creates a realm like initialize_realm() does, but with the this value of its global environment that the callback
    // creates, if there is one, and returns the completion of the realm's creation.
    JSCompletion initialize_realm_with_global_this_value(JSCreateRealmObjectCallback create_global_this_value, void* create_global_this_value_context)
    {
        VERIFY(!realm());
        return js_realm_initialize_host_defined_realm(vm(), m_realm_execution_context, nullptr, nullptr, create_global_this_value, create_global_this_value_context);
    }

    u8 const* realm_execution_context() const { return m_realm_execution_context; }

    // The realm is a field of its execution context, which the embedder reads through the layout of the runtime.
    JSRealm* realm() const
    {
        JSRealm* realm = nullptr;
        __builtin_memcpy(&realm, m_realm_execution_context + JS_LAYOUT_EXECUTION_CONTEXT_REALM_OFFSET, sizeof(realm));
        return realm;
    }

    JSObject* global_object() const { return js_realm_global_object(realm()); }
    JSEnvironment* global_environment() const { return js_realm_global_environment(realm()); }
    JSObject* intrinsic(JSIntrinsic intrinsic) { return js_realm_intrinsic(vm(), realm(), intrinsic); }

    JSCompletion evaluate(StringView source, StringView source_name = "test.js"sv)
    {
        return js_script_evaluate(vm(), realm(), ascii_view(source), ascii_view(source_name));
    }

    // The value of a script that must complete normally.
    JSValue value_of(StringView source)
    {
        auto completion = evaluate(source);
        VERIFY(completion.variant == JS_COMPLETION_NORMAL);
        return completion.payload;
    }

    // The object that a script, which must complete normally, evaluates to.
    JSObject* object_of(StringView source)
    {
        auto* object = object_of_value(value_of(source));
        VERIFY(object);
        return object;
    }

    // Runs a script that reports what it finds wrong by throwing.
    bool run(StringView source)
    {
        return evaluate(source).variant == JS_COMPLETION_NORMAL;
    }

    void collect_garbage() { js_vm_collect_garbage(vm()); }

private:
    explicit EmbeddedVM(JSVmOptions options)
    {
        VERIFY(js_vm_construct_at(m_storage, sizeof(m_storage), alignof(EmbeddedVM), &options));
    }

    alignas(JS_LAYOUT_VM_ALIGN) u8 m_storage[JS_LAYOUT_VM_SIZE];
    alignas(JS_LAYOUT_EXECUTION_CONTEXT_ALIGN) u8 m_realm_execution_context[JS_LAYOUT_EXECUTION_CONTEXT_SIZE] {};
};

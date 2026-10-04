/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Noncopyable.h>
#include <AK/NonnullOwnPtr.h>
#include <AK/StringView.h>
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

// The pointer that the payload of a normal completion carries, such as the cell that an operation creates.
template<typename T>
T* pointer_of_payload(JSCompletion completion)
{
    VERIFY(completion.variant == JS_COMPLETION_NORMAL);
    return reinterpret_cast<T*>(static_cast<uintptr_t>(completion.payload));
}

// The tag of a JSValue that holds an object, as JS::Value encodes it in both runtimes, with the object's offset in the
// heap region below it.
constexpr u64 object_value_tag = 0b001 | GC::IS_CELL_BIT;

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
    static NonnullOwnPtr<EmbeddedVM> create(JSVmOptions options = {})
    {
        return adopt_own(*new EmbeddedVM(options));
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
        VERIFY(!realm());
        auto completion = js_realm_initialize_host_defined_realm(vm(), m_realm_execution_context, nullptr, nullptr, nullptr, nullptr);
        VERIFY(completion.variant == JS_COMPLETION_NORMAL);
    }

    u8 const* realm_execution_context() const { return m_realm_execution_context; }

    // The realm is a field of its execution context, which the embedder reads through the layout of the runtime.
    JSRealm* realm() const
    {
        JSRealm* realm = nullptr;
        __builtin_memcpy(&realm, m_realm_execution_context + JS_LAYOUT_EXECUTION_CONTEXT_REALM_OFFSET, sizeof(realm));
        return realm;
    }

    JSCompletion evaluate(StringView source, StringView source_name = "test.js"sv)
    {
        return js_script_evaluate(vm(), realm(), ascii_view(source), ascii_view(source_name));
    }

    // Runs a script that reports what it finds wrong by throwing.
    bool run(StringView source)
    {
        return evaluate(source).variant == JS_COMPLETION_NORMAL;
    }

private:
    explicit EmbeddedVM(JSVmOptions options)
    {
        VERIFY(js_vm_construct_at(m_storage, sizeof(m_storage), alignof(EmbeddedVM), &options));
    }

    alignas(JS_LAYOUT_VM_ALIGN) u8 m_storage[JS_LAYOUT_VM_SIZE];
    alignas(JS_LAYOUT_EXECUTION_CONTEXT_ALIGN) u8 m_realm_execution_context[JS_LAYOUT_EXECUTION_CONTEXT_SIZE] {};
};

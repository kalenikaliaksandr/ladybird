/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Noncopyable.h>
#include <AK/NonnullOwnPtr.h>
#include <LibJS/Embedding/ABI.h>
#include <LibJS/Embedding/Layout.h>
#include <LibJS/HostObjectABI.h>
#include <LibTest/TestCase.h>

// The pointer that the payload of a normal completion carries, such as the cell that an operation creates.
template<typename T>
T* pointer_of_payload(JSCompletion completion)
{
    VERIFY(completion.variant == JS_COMPLETION_NORMAL);
    return reinterpret_cast<T*>(static_cast<uintptr_t>(completion.payload));
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

private:
    explicit EmbeddedVM(JSVmOptions options)
    {
        VERIFY(js_vm_construct_at(m_storage, sizeof(m_storage), alignof(EmbeddedVM), &options));
    }

    alignas(JS_LAYOUT_VM_ALIGN) u8 m_storage[JS_LAYOUT_VM_SIZE];
};

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibGC/Heap.h>

#include "EmbeddingTest.h"

TEST_CASE(a_vm_in_embedder_storage_can_make_its_heap_the_process_default)
{
    auto embedded_vm = EmbeddedVM::create({ .become_process_default_heap = true, .shared_memory_shared_array_buffers = false });
    auto* heap = reinterpret_cast<GC::Heap*>(js_vm_heap(embedded_vm->vm()));
    EXPECT_EQ(heap, &GC::Heap::the());

    // The heap gathers its roots from the VM at the address it was constructed at.
    js_vm_collect_garbage(embedded_vm->vm());
    heap->collect_garbage();
}

TEST_CASE(a_vm_needs_storage_of_its_size_and_alignment)
{
    alignas(JS_LAYOUT_VM_ALIGN) static u8 storage[JS_LAYOUT_VM_SIZE + JS_LAYOUT_VM_ALIGN];
    EXPECT(!js_vm_construct_at(nullptr, sizeof(storage), JS_LAYOUT_VM_ALIGN, nullptr));
    EXPECT(!js_vm_construct_at(storage, 64, JS_LAYOUT_VM_ALIGN, nullptr));
    EXPECT(!js_vm_construct_at(storage, sizeof(storage), 1, nullptr));
    EXPECT(!js_vm_construct_at(storage + 1, JS_LAYOUT_VM_SIZE, JS_LAYOUT_VM_ALIGN, nullptr));

    EXPECT(js_vm_construct_at(storage, JS_LAYOUT_VM_SIZE, JS_LAYOUT_VM_ALIGN, nullptr));
    auto* vm = reinterpret_cast<JSVM*>(storage);
    js_vm_finish_execution_generation(vm);
    js_vm_collect_garbage(vm);
    js_vm_destroy_at(vm);
}

TEST_CASE(a_realm_of_the_embedder_evaluates_scripts)
{
    auto embedded_vm = EmbeddedVM::create();
    embedded_vm->initialize_realm();
    EXPECT(embedded_vm->realm() != nullptr);
    u8 const* running_execution_context = nullptr;
    __builtin_memcpy(&running_execution_context, reinterpret_cast<u8 const*>(embedded_vm->vm()) + JS_LAYOUT_VM_RUNNING_EXECUTION_CONTEXT_OFFSET, sizeof(running_execution_context));
    EXPECT_EQ(running_execution_context, embedded_vm->realm_execution_context());

    EXPECT(embedded_vm->run("if (1 + 1 !== 2) throw new Error();"sv));
    EXPECT(embedded_vm->run("var counter = 41;"sv));
    js_vm_collect_garbage(embedded_vm->vm());
    EXPECT(embedded_vm->run("if (++counter !== 42 || typeof globalThis.Array !== 'function') throw new Error();"sv));

    EXPECT_EQ(embedded_vm->evaluate("throw new TypeError();"sv).variant, JS_COMPLETION_THROW);
    EXPECT_EQ(embedded_vm->evaluate("let = = ;"sv).variant, JS_COMPLETION_THROW);
    // A global lexical declaration of an earlier script conflicts with one of a later script before the later runs.
    EXPECT(embedded_vm->run("let declared_by_two_scripts;"sv));
    EXPECT_EQ(embedded_vm->evaluate("let declared_by_two_scripts;"sv).variant, JS_COMPLETION_THROW);
}

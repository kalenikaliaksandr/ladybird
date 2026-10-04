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

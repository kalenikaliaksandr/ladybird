/*
 * Copyright (c) 2024-2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/AtomicRefCounted.h>
#include <AK/Noncopyable.h>

#include <cstddef>
#include <chrono>

#ifdef USE_VULKAN
#    include <LibGfx/VulkanContext.h>
#endif

#ifdef AK_OS_MACOS
#    include <LibGfx/MetalContext.h>
#endif

class SkSurface;

namespace skgpu::graphite {
class Context;
class Recorder;
}

namespace Gfx {

struct VulkanContext;
class MetalContext;

class SkiaBackendContext : public AtomicRefCounted<SkiaBackendContext> {
    AK_MAKE_NONCOPYABLE(SkiaBackendContext);
    AK_MAKE_NONMOVABLE(SkiaBackendContext);

public:
#ifdef USE_VULKAN
    static RefPtr<SkiaBackendContext> create_vulkan_context(const VulkanContext& vulkan_context);
#endif

#ifdef AK_OS_MACOS
    static RefPtr<SkiaBackendContext> create_metal_context(NonnullRefPtr<MetalContext>);
#endif

    static void initialize_gpu_backend();
    static RefPtr<SkiaBackendContext> create_independent_gpu_backend();
    static RefPtr<SkiaBackendContext> the_main_thread_context();

    SkiaBackendContext() { }
    virtual ~SkiaBackendContext() { }

    virtual void flush_and_submit(SkSurface*) { }
    virtual skgpu::graphite::Context* graphite_context() const = 0;
    virtual skgpu::graphite::Recorder* recorder() const = 0;
    virtual void perform_deferred_cleanup(std::chrono::milliseconds) { }
    virtual size_t resource_cache_bytes() const { return 0; }
    virtual void free_gpu_resources() { }

    virtual MetalContext& metal_context() = 0;
    virtual VulkanContext const& vulkan_context() = 0;
};

}

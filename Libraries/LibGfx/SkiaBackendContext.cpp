/*
 * Copyright (c) 2024-2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/NonnullOwnPtr.h>
#include <AK/RefPtr.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/SkiaBackendContext.h>

#include <core/SkSurface.h>

#pragma push_macro("TODO")
#undef TODO
#include <gpu/graphite/Context.h>
#include <gpu/graphite/ContextOptions.h>
#include <gpu/graphite/GraphiteTypes.h>
#include <gpu/graphite/Recorder.h>
#pragma pop_macro("TODO")

#include <memory>

#ifdef USE_VULKAN
#    pragma push_macro("TODO")
#    undef TODO
#    include <gpu/graphite/vk/VulkanGraphiteContext.h>
#    include <gpu/vk/VulkanBackendContext.h>
#    include <gpu/vk/VulkanExtensions.h>
#    pragma pop_macro("TODO")
#endif

#ifdef AK_OS_MACOS
#    pragma push_macro("TODO")
#    undef TODO
#    include <gpu/graphite/mtl/MtlBackendContext.h>
#    pragma pop_macro("TODO")
#endif

namespace Gfx {

#if defined(AK_OS_MACOS) || USE_VULKAN
static constexpr size_t skia_resource_cache_limit = 256 * MiB;
#endif

static RefPtr<SkiaBackendContext> s_main_thread_context;

void SkiaBackendContext::initialize_gpu_backend()
{
    VERIFY(!s_main_thread_context);

    s_main_thread_context = create_independent_gpu_backend();
}

RefPtr<SkiaBackendContext> SkiaBackendContext::create_independent_gpu_backend()
{
#ifdef AK_OS_MACOS
    auto metal_context = get_metal_context();
    if (!metal_context)
        return {};
    return create_metal_context(*metal_context);
#elif USE_VULKAN
    auto maybe_vulkan_context = Gfx::create_vulkan_context();
    if (maybe_vulkan_context.is_error()) {
        dbgln("Vulkan context creation failed: {}", maybe_vulkan_context.error());
        return {};
    }
    auto vulkan_context = maybe_vulkan_context.release_value();
    return create_vulkan_context(vulkan_context);
#else
    return {};
#endif
}

RefPtr<SkiaBackendContext> SkiaBackendContext::the_main_thread_context()
{
    return s_main_thread_context;
}

#ifdef USE_VULKAN
class SkiaVulkanBackendContext final : public SkiaBackendContext {
    AK_MAKE_NONCOPYABLE(SkiaVulkanBackendContext);
    AK_MAKE_NONMOVABLE(SkiaVulkanBackendContext);

public:
    SkiaVulkanBackendContext(std::unique_ptr<skgpu::graphite::Context> context, std::unique_ptr<skgpu::graphite::Recorder> recorder, VulkanContext const& vulkan_context, NonnullOwnPtr<skgpu::VulkanExtensions> extensions)
        : m_context(move(context))
        , m_recorder(move(recorder))
        , m_extensions(move(extensions))
        , m_vulkan_context(vulkan_context)
    {
    }

    ~SkiaVulkanBackendContext() override
    {
        m_recorder.reset();
        m_context.reset();
#    ifdef USE_VULKAN_DMABUF_IMAGES
        if (m_vulkan_context.command_pool != VK_NULL_HANDLE)
            vkDestroyCommandPool(m_vulkan_context.logical_device, m_vulkan_context.command_pool, nullptr);
#    endif
        if (m_vulkan_context.logical_device != VK_NULL_HANDLE)
            vkDestroyDevice(m_vulkan_context.logical_device, nullptr);
        if (m_vulkan_context.instance != VK_NULL_HANDLE)
            vkDestroyInstance(m_vulkan_context.instance, nullptr);
    }

    void flush_and_submit(SkSurface* surface) override
    {
        (void)surface;
        auto recording = m_recorder->snap();
        if (!recording)
            return;

        skgpu::graphite::InsertRecordingInfo info;
        info.fRecording = recording.get();
        if (m_context->insertRecording(info) != skgpu::graphite::InsertStatus::kSuccess)
            return;

        m_context->submit(skgpu::graphite::SyncToCpu::kYes);
        m_context->checkAsyncWorkCompletion();
    }

    skgpu::VulkanExtensions const* extensions() const { return m_extensions.ptr(); }

    skgpu::graphite::Context* graphite_context() const override { return m_context.get(); }
    skgpu::graphite::Recorder* recorder() const override { return m_recorder.get(); }

    void perform_deferred_cleanup(std::chrono::milliseconds ms_not_used) override
    {
        m_recorder->performDeferredCleanup(ms_not_used);
        m_context->performDeferredCleanup(ms_not_used);
    }

    size_t resource_cache_bytes() const override
    {
        return m_recorder->currentBudgetedBytes() + m_context->currentBudgetedBytes();
    }

    void free_gpu_resources() override
    {
        m_recorder->freeGpuResources();
        m_context->freeGpuResources();
    }

    VulkanContext const& vulkan_context() override { return m_vulkan_context; }

    MetalContext& metal_context() override { VERIFY_NOT_REACHED(); }

private:
    std::unique_ptr<skgpu::graphite::Context> m_context;
    std::unique_ptr<skgpu::graphite::Recorder> m_recorder;
    NonnullOwnPtr<skgpu::VulkanExtensions> m_extensions;
    VulkanContext const m_vulkan_context;
};

RefPtr<SkiaBackendContext> SkiaBackendContext::create_vulkan_context(VulkanContext const& vulkan_context)
{
    skgpu::VulkanBackendContext backend_context;

    backend_context.fInstance = vulkan_context.instance;
    backend_context.fDevice = vulkan_context.logical_device;
    backend_context.fQueue = vulkan_context.graphics_queue;
    backend_context.fGraphicsQueueIndex = vulkan_context.graphics_queue_family;
    backend_context.fPhysicalDevice = vulkan_context.physical_device;
    backend_context.fMaxAPIVersion = vulkan_context.api_version;
    backend_context.fGetProc = [](char const* proc_name, VkInstance instance, VkDevice device) {
        if (device != VK_NULL_HANDLE) {
            return vkGetDeviceProcAddr(device, proc_name);
        }
        return vkGetInstanceProcAddr(instance, proc_name);
    };

    auto extensions = make<skgpu::VulkanExtensions>();
    backend_context.fVkExtensions = extensions.ptr();

    skgpu::graphite::ContextOptions options;
    auto ctx = skgpu::graphite::ContextFactory::MakeVulkan(backend_context, options);
    VERIFY(ctx);
    ctx->setMaxBudgetedBytes(skia_resource_cache_limit);

    auto recorder = ctx->makeRecorder();
    VERIFY(recorder);
    recorder->setMaxBudgetedBytes(skia_resource_cache_limit);

    return adopt_ref(*new SkiaVulkanBackendContext(move(ctx), move(recorder), vulkan_context, move(extensions)));
}
#endif

#ifdef AK_OS_MACOS
class SkiaMetalBackendContext final : public SkiaBackendContext {
    AK_MAKE_NONCOPYABLE(SkiaMetalBackendContext);
    AK_MAKE_NONMOVABLE(SkiaMetalBackendContext);

public:
    SkiaMetalBackendContext(std::unique_ptr<skgpu::graphite::Context> context, std::unique_ptr<skgpu::graphite::Recorder> recorder, NonnullRefPtr<MetalContext> metal_context)
        : m_context(move(context))
        , m_recorder(move(recorder))
        , m_metal_context(move(metal_context))
    {
    }

    ~SkiaMetalBackendContext() override
    {
        m_recorder.reset();
        m_context.reset();
    }

    void flush_and_submit(SkSurface* surface) override
    {
        (void)surface;
        auto recording = m_recorder->snap();
        if (!recording)
            return;

        skgpu::graphite::InsertRecordingInfo info;
        info.fRecording = recording.get();
        if (m_context->insertRecording(info) != skgpu::graphite::InsertStatus::kSuccess)
            return;

        m_context->submit(skgpu::graphite::SyncToCpu::kYes);
        m_context->checkAsyncWorkCompletion();
    }

    skgpu::graphite::Context* graphite_context() const override { return m_context.get(); }
    skgpu::graphite::Recorder* recorder() const override { return m_recorder.get(); }

    void perform_deferred_cleanup(std::chrono::milliseconds ms_not_used) override
    {
        m_recorder->performDeferredCleanup(ms_not_used);
        m_context->performDeferredCleanup(ms_not_used);
    }

    size_t resource_cache_bytes() const override
    {
        return m_recorder->currentBudgetedBytes() + m_context->currentBudgetedBytes();
    }

    void free_gpu_resources() override
    {
        m_recorder->freeGpuResources();
        m_context->freeGpuResources();
    }

    VulkanContext const& vulkan_context() override { VERIFY_NOT_REACHED(); }

    MetalContext& metal_context() override { return m_metal_context; }

private:
    std::unique_ptr<skgpu::graphite::Context> m_context;
    std::unique_ptr<skgpu::graphite::Recorder> m_recorder;
    NonnullRefPtr<MetalContext> m_metal_context;
};

RefPtr<SkiaBackendContext> SkiaBackendContext::create_metal_context(NonnullRefPtr<MetalContext> metal_context)
{
    skgpu::graphite::MtlBackendContext backend_context;
    backend_context.fDevice.retain(metal_context->device());
    backend_context.fQueue.retain(metal_context->queue());
    skgpu::graphite::ContextOptions options;
    auto ctx = skgpu::graphite::ContextFactory::MakeMetal(backend_context, options);
    VERIFY(ctx);
    ctx->setMaxBudgetedBytes(skia_resource_cache_limit);

    auto recorder = ctx->makeRecorder();
    VERIFY(recorder);
    recorder->setMaxBudgetedBytes(skia_resource_cache_limit);

    return adopt_ref(*new SkiaMetalBackendContext(move(ctx), move(recorder), move(metal_context)));
}
#endif

}

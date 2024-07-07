/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#if !defined(USE_VULKAN)
static_assert(false, "This file must only be used when Vulkan is available");
#endif

#include <AK/Forward.h>
#include <AK/Function.h>
#include <AK/RefCounted.h>
#include <LibCore/VulkanSharedMemoryDescriptor.h>
#include <LibIPC/Forward.h>
#include <vulkan/vulkan.h>

namespace Core {

class VulkanContext : public RefCounted<VulkanContext> {
public:
    static ErrorOr<NonnullRefPtr<VulkanContext>> create();

    uint32_t api_version() const { return m_api_version; }
    VkInstance instance() const { return m_instance; }
    VkPhysicalDevice physical_device() const { return m_physical_device; }
    VkDevice device() const { return m_device; }
    VkQueue graphics_queue() const { return m_graphics_queue; }

private:
    VulkanContext(uint32_t api_version, VkInstance instance, VkPhysicalDevice physical_device, VkDevice device, VkQueue graphics_queue)
        : m_api_version(api_version)
        , m_instance(instance)
        , m_physical_device(physical_device)
        , m_device(device)
        , m_graphics_queue(graphics_queue)
    {
    }

    uint32_t m_api_version { VK_API_VERSION_1_0 };
    VkInstance m_instance { VK_NULL_HANDLE };
    VkPhysicalDevice m_physical_device { VK_NULL_HANDLE };
    VkDevice m_device { VK_NULL_HANDLE };
    VkQueue m_graphics_queue { VK_NULL_HANDLE };
};

class VulkanImage : public RefCounted<VulkanImage> {
public:
    static NonnullRefPtr<VulkanImage> create(VkDevice device, VkPhysicalDevice physical_device, int width, int height);
    static NonnullRefPtr<VulkanImage> create_from_fd(int fd, uint64_t allocation_size, VkDevice, int width, int height);

    VkImage image() const { return m_image; }

    int width() const { return m_width; }
    int height() const { return m_height; }

    //    int fd() const { return m_fd; }
    VulkanSharedMemoryDescriptor descriptor() const;

    void* map();

private:
    VulkanImage(int width, int height, VkImage image, VkDeviceMemory device_memory, int fd, uint64_t allocation_size, VkDevice device)
        : m_image(image)
        , m_device_memory(device_memory)
        , m_fd(fd)
        , m_width(width)
        , m_height(height)
        , m_allocation_size(allocation_size)
        , m_device(device)
    {
    }

    VkImage m_image { VK_NULL_HANDLE };
    VkDeviceMemory m_device_memory { VK_NULL_HANDLE };
    int m_fd { -1 };

    int m_width { 0 };
    int m_height { 0 };

    uint64_t m_allocation_size { 0 };

    VkDevice m_device { VK_NULL_HANDLE };
};

class VulkanMemory : public RefCounted<VulkanMemory> {
public:
    //    static NonnullRefPtr<VulkanMemory> create_from_fd(int fd, uint64_t allocation_size, VkDevice);

    //    void* map();

private:
    VulkanMemory(VkDeviceMemory device_memory, VkDevice device, uint64_t allocation_size)
        : m_device_memory(device_memory)
        , m_device(device)
        , m_allocation_size(allocation_size)
    {
    }

    VkDeviceMemory m_device_memory { VK_NULL_HANDLE };
    VkDevice m_device { VK_NULL_HANDLE };

    uint64_t m_allocation_size { 0 };
};

}

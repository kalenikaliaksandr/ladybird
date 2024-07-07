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
    static VulkanImage create_from_fd(int fd);

    VkImage image() const { return m_image; }

    int width() const { return m_width; }
    int height() const { return m_height; }

    int fd() const { return m_fd; }

private:
    VulkanImage(int width, int height, VkImage image, VkDeviceMemory device_memory, int fd)
        : m_image(image)
        , m_device_memory(device_memory)
        , m_fd(fd)
        , m_width(width)
        , m_height(height)
    {
    }

    VkImage m_image { VK_NULL_HANDLE };
    VkDeviceMemory m_device_memory { VK_NULL_HANDLE };
    int m_fd { -1 };

    int m_width { 0 };
    int m_height { 0 };
};

}

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
#include <vulkan/vulkan.h>

namespace Core {

struct VulkanContext {
    uint32_t api_version { VK_API_VERSION_1_0 };
    VkInstance instance { VK_NULL_HANDLE };
    VkPhysicalDevice physical_device { VK_NULL_HANDLE };
    VkDevice logical_device { VK_NULL_HANDLE };
    VkQueue graphics_queue { VK_NULL_HANDLE };
};

ErrorOr<VulkanContext> create_vulkan_context();

class VulkanImage {
    AK_MAKE_NONCOPYABLE(VulkanImage);

public:
    static VulkanImage create(VkDevice device, VkPhysicalDevice physical_device, int width, int height);

    VulkanImage(VulkanImage&& other) = default;
    VulkanImage& operator=(VulkanImage&& other) = default;

    VkImage image() const { return m_image; }

    int width() const { return m_width; }
    int height() const { return m_height; }

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

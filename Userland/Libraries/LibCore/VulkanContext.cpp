/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Format.h>
#include <AK/Vector.h>
#include <LibCore/VulkanContext.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibIPC/File.h>

namespace Core {

ErrorOr<VkInstance> create_instance(uint32_t api_version)
{
    VkInstance instance;

    VkApplicationInfo app_info {};
    app_info.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO;
    app_info.pApplicationName = "Ladybird";
    app_info.applicationVersion = VK_MAKE_VERSION(1, 0, 0);
    app_info.pEngineName = nullptr;
    app_info.engineVersion = VK_MAKE_VERSION(1, 0, 0);
    app_info.apiVersion = api_version;

    VkInstanceCreateInfo create_info {};
    create_info.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO;
    create_info.pApplicationInfo = &app_info;

    auto result = vkCreateInstance(&create_info, nullptr, &instance);
    if (result != VK_SUCCESS) {
        dbgln("vkCreateInstance returned {}", to_underlying(result));
        return Error::from_string_view("Application instance creation failed"sv);
    }

    return instance;
}

ErrorOr<VkPhysicalDevice> pick_physical_device(VkInstance instance)
{
    uint32_t device_count = 0;
    vkEnumeratePhysicalDevices(instance, &device_count, nullptr);

    if (device_count == 0)
        return Error::from_string_view("Can't find any physical devices available"sv);

    Vector<VkPhysicalDevice> devices;
    devices.resize(device_count);
    vkEnumeratePhysicalDevices(instance, &device_count, devices.data());

    VkPhysicalDevice picked_device = VK_NULL_HANDLE;
    // Pick discrete GPU or the first device in the list
    for (auto const& device : devices) {
        if (picked_device == VK_NULL_HANDLE)
            picked_device = device;

        VkPhysicalDeviceProperties device_properties;
        vkGetPhysicalDeviceProperties(device, &device_properties);
        if (device_properties.deviceType == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU)
            picked_device = device;
    }

    if (picked_device != VK_NULL_HANDLE)
        return picked_device;

    VERIFY_NOT_REACHED();
}

ErrorOr<VkDevice> create_logical_device(VkPhysicalDevice physical_device)
{
    VkDevice device;

    uint32_t queue_family_count = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(physical_device, &queue_family_count, nullptr);
    Vector<VkQueueFamilyProperties> queue_families;
    queue_families.resize(queue_family_count);
    vkGetPhysicalDeviceQueueFamilyProperties(physical_device, &queue_family_count, queue_families.data());

    int graphics_queue_family_index = -1;
    for (int i = 0; i < static_cast<int>(queue_families.size()); i++) {
        if (queue_families[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) {
            graphics_queue_family_index = i;
            break;
        }
    }

    VkDeviceQueueCreateInfo queue_create_info {};
    queue_create_info.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO;
    queue_create_info.queueFamilyIndex = graphics_queue_family_index;
    queue_create_info.queueCount = 1;

    float const queue_priority = 1.0f;
    queue_create_info.pQueuePriorities = &queue_priority;

    VkPhysicalDeviceFeatures deviceFeatures {};

    VkDeviceCreateInfo create_device_info {};
    create_device_info.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO;
    create_device_info.pQueueCreateInfos = &queue_create_info;
    create_device_info.queueCreateInfoCount = 1;
    create_device_info.pEnabledFeatures = &deviceFeatures;

    create_device_info.enabledExtensionCount = 1;
    char const* deviceExtensions[] = { VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME };
    create_device_info.ppEnabledExtensionNames = deviceExtensions;

    if (vkCreateDevice(physical_device, &create_device_info, nullptr, &device) != VK_SUCCESS) {
        return Error::from_string_view("Logical device creation failed"sv);
    }

    return device;
}

ErrorOr<VulkanContext> create_vulkan_context()
{
    uint32_t const api_version = VK_API_VERSION_1_0;
    auto* instance = TRY(create_instance(api_version));
    auto* physical_device = TRY(pick_physical_device(instance));
    auto* logical_device = TRY(create_logical_device(physical_device));

    VkQueue graphics_queue;
    vkGetDeviceQueue(logical_device, 0, 0, &graphics_queue);

    return VulkanContext {
        .api_version = api_version,
        .instance = instance,
        .physical_device = physical_device,
        .logical_device = logical_device,
        .graphics_queue = graphics_queue,
    };
}

VulkanImage VulkanImage::create(VkDevice device, VkPhysicalDevice physical_device, int width, int height)
{
    VkImageCreateInfo image_create_info = {};
    image_create_info.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
    image_create_info.pNext = nullptr;
    image_create_info.flags = 0;
    image_create_info.imageType = VK_IMAGE_TYPE_2D;      // 1D, 2D, or 3D image
    image_create_info.format = VK_FORMAT_B8G8R8A8_UNORM; // Format of the image
    image_create_info.extent.width = width;
    image_create_info.extent.height = height;
    image_create_info.extent.depth = 1;
    image_create_info.mipLevels = 1;
    image_create_info.arrayLayers = 1;
    image_create_info.samples = VK_SAMPLE_COUNT_1_BIT;                                          // Number of samples per pixel
    image_create_info.tiling = VK_IMAGE_TILING_OPTIMAL;                                         // Tiling arrangement
    image_create_info.usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_SAMPLED_BIT; // Usage flags
    image_create_info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;                                  // Sharing mode
    image_create_info.queueFamilyIndexCount = 0;                                                // Number of queue families
    image_create_info.pQueueFamilyIndices = nullptr;
    image_create_info.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;

    VkImage image;
    VkResult result = vkCreateImage(device, &image_create_info, nullptr, &image);
    if (result != VK_SUCCESS) {
        VERIFY_NOT_REACHED();
    }

    VkMemoryRequirements memory_requirements;
    vkGetImageMemoryRequirements(device, image, &memory_requirements);

    VkPhysicalDeviceMemoryProperties memory_properties;
    vkGetPhysicalDeviceMemoryProperties(physical_device, &memory_properties);
    (void)memory_properties;

    VkExportMemoryAllocateInfo exportInfo = {};
    exportInfo.sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO;
    exportInfo.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;

    VkMemoryAllocateInfo memory_allocate_info = {};
    memory_allocate_info.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
    memory_allocate_info.pNext = &exportInfo;
    memory_allocate_info.allocationSize = memory_requirements.size;
    memory_allocate_info.memoryTypeIndex = 0; // FIXME

    VkDeviceMemory imageMemory;
    result = vkAllocateMemory(device, &memory_allocate_info, nullptr, &imageMemory);
    if (result != VK_SUCCESS) {
        VERIFY_NOT_REACHED();
    }

    result = vkBindImageMemory(device, image, imageMemory, 0);
    if (result != VK_SUCCESS) {
        VERIFY_NOT_REACHED();
    }

    auto vkGetMemoryFdKHR = (PFN_vkGetMemoryFdKHR)vkGetDeviceProcAddr(device, "vkGetMemoryFdKHR");
    if (!vkGetMemoryFdKHR) {
        VERIFY_NOT_REACHED();
    }

    VkMemoryGetFdInfoKHR memoryGetFdInfo = {};
    memoryGetFdInfo.sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR;
    memoryGetFdInfo.pNext = NULL;
    memoryGetFdInfo.memory = imageMemory;                                      // Vulkan memory object
    memoryGetFdInfo.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT; // Desired handle type

    int fd = -1;
    vkGetMemoryFdKHR(device, &memoryGetFdInfo, &fd);
    dbgln(">>>fd={}", fd);

    return VulkanImage(width, height, image, imageMemory, fd);
}

VulkanImage VulkanImage::create_from_fd(int fd)
{
    (void)fd;
    VERIFY_NOT_REACHED();
}

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Core::VulkanImage const& vulkan_image)
{
    TRY(encoder.encode(TRY(IPC::File::clone_fd(vulkan_image.fd()))));
    return {};
}

template<>
ErrorOr<Core::VulkanImage> decode(Decoder& decoder)
{
    (void)decoder;
    VERIFY_NOT_REACHED();

    //     if (auto valid = TRY(decoder.decode<bool>()); !valid)
    //         return Gfx::ShareableBitmap {};
    //
    //     auto anon_file = TRY(decoder.decode<IPC::File>());
    //     auto size = TRY(decoder.decode<Gfx::IntSize>());
    //     auto raw_bitmap_format = TRY(decoder.decode<u32>());
    //     if (!Gfx::is_valid_bitmap_format(raw_bitmap_format))
    //         return Error::from_string_literal("IPC: Invalid Gfx::ShareableBitmap format");
    //
    //     auto bitmap_format = static_cast<Gfx::BitmapFormat>(raw_bitmap_format);
    //
    //     auto buffer = TRY(Core::AnonymousBuffer::create_from_anon_fd(anon_file.take_fd(), Gfx::Bitmap::size_in_bytes(Gfx::Bitmap::minimum_pitch(size.width(), bitmap_format), size.height())));
    //     auto bitmap = TRY(Gfx::Bitmap::create_with_anonymous_buffer(bitmap_format, move(buffer), size));
    //
    //     return Gfx::ShareableBitmap { move(bitmap), Gfx::ShareableBitmap::ConstructWithKnownGoodBitmap };
}

}

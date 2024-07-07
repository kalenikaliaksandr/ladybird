#pragma once

#include <LibIPC/Forward.h>

namespace Core {

struct VulkanSharedMemoryDescriptor {
    int fd { -1 };
    uint64_t allocation_size { 0 };
    int width { 0 };
    int height { 0 };
};

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder&, Core::VulkanSharedMemoryDescriptor const&);

template<>
ErrorOr<Core::VulkanSharedMemoryDescriptor> decode(Decoder&);

}

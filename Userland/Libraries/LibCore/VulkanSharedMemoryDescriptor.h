#pragma once

#include <LibIPC/Forward.h>

namespace Core {

struct VulkanSharedMemoryDescriptor {
    int fd { -1 };
};

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder&, Core::VulkanSharedMemoryDescriptor const&);

template<>
ErrorOr<Core::VulkanSharedMemoryDescriptor> decode(Decoder&);

}

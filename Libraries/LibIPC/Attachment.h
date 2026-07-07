/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/Noncopyable.h>
#include <AK/Platform.h>

#if defined(AK_OS_MACOS)
#    include <LibCore/MachPort.h>
#else
#    include <LibCore/SystemHandle.h>
#endif

namespace IPC {

class Attachment {
    AK_MAKE_NONCOPYABLE(Attachment);

public:
    Attachment() = default;

#if defined(AK_OS_MACOS)
    Attachment(Attachment&&);
    Attachment& operator=(Attachment&&);
    ~Attachment();

    static Attachment from_fd(int fd);
    int to_fd();

    static Attachment from_mach_port(Core::MachPort, Core::MachPort::MessageRight);
    Core::MachPort const& mach_port() const { return m_port; }
    Core::MachPort::MessageRight message_right() const { return m_message_right; }
    Core::MachPort release_mach_port();
#else
    Attachment(Attachment&&) = default;
    Attachment& operator=(Attachment&&) = default;
    ~Attachment() = default;

    // NOTE: This tags the fd HandleKind::File; use from_handle() for anything that is not a plain file.
    static Attachment from_fd(int fd);
    int to_fd() { return static_cast<int>(m_handle.leak().raw_value()); }

    static Attachment from_handle(Core::SystemHandle);
    Core::SystemHandle take_handle() { return move(m_handle); }
#endif

private:
#if defined(AK_OS_MACOS)
    Core::MachPort m_port;
    Core::MachPort::MessageRight m_message_right { Core::MachPort::MessageRight::MoveSend };
#else
    Core::SystemHandle m_handle;
#endif
};

}

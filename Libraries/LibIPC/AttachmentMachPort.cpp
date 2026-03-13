/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Format.h>
#include <LibCore/MachPort.h>
#include <LibCore/System.h>
#include <LibIPC/Attachment.h>

extern "C" {
int fileport_makeport(int fd, mach_port_t* port);
int fileport_makefd(mach_port_t port);
}

namespace IPC {

Attachment::Attachment(Attachment&& other)
    : m_port(move(other.m_port))
    , m_message_right(other.m_message_right)
{
}

Attachment& Attachment::operator=(Attachment&& other)
{
    if (this != &other) {
        m_port = move(other.m_port);
        m_message_right = other.m_message_right;
    }
    return *this;
}

Attachment::~Attachment() = default;

Attachment Attachment::from_fd(int fd)
{
    mach_port_t port = MACH_PORT_NULL;
    if (fileport_makeport(fd, &port) != 0) {
        dbgln("Attachment::from_fd: fileport_makeport failed for fd {}", fd);
        port = MACH_PORT_NULL;
    }
    (void)Core::System::close(fd);

    Attachment attachment;
    attachment.m_port = Core::MachPort::adopt_right(port, Core::MachPort::PortRight::Send);
    attachment.m_message_right = Core::MachPort::MessageRight::MoveSend;
    return attachment;
}

int Attachment::to_fd()
{
    if (!MACH_PORT_VALID(m_port.port()))
        return -1;

    int fd = fileport_makefd(m_port.port());
    // Deallocate the port right after converting to fd
    mach_port_deallocate(mach_task_self(), m_port.release());
    return fd;
}

Attachment Attachment::from_mach_port(Core::MachPort port, Core::MachPort::MessageRight right)
{
    Attachment attachment;
    attachment.m_port = move(port);
    attachment.m_message_right = right;
    return attachment;
}

Core::MachPort Attachment::release_mach_port()
{
    return move(m_port);
}

}

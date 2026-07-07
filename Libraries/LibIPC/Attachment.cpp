/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibIPC/Attachment.h>

namespace IPC {

Attachment Attachment::from_handle(Core::SystemHandle handle)
{
    Attachment attachment;
    attachment.m_handle = move(handle);
    return attachment;
}

Attachment Attachment::from_fd(int fd)
{
    // NOTE: This tags the fd HandleKind::File; use from_handle() for anything that is not a plain file.
    return from_handle(Core::SystemHandle::adopt(Core::SystemHandleRef::from_raw(fd, Core::HandleKind::File)));
}

}

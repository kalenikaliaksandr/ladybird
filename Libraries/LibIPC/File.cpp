/*
 * Copyright (c) 2020, Sergey Bugaev <bugaevc@serenityos.org>
 * Copyright (c) 2021, Andreas Kling <andreas@ladybird.org>
 * Copyright (c) 2023-2025, Tim Flynn <trflynn89@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibCore/File.h>
#include <LibCore/System.h>
#include <LibIPC/Attachment.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/File.h>

namespace IPC {

File File::adopt_file(NonnullOwnPtr<Core::File> file)
{
    return File(file->leak_handle());
}

File File::adopt_handle(Core::SystemHandle handle)
{
    return File(move(handle));
}

File File::adopt_fd(int fd)
{
    return File(Core::SystemHandle::adopt(Core::SystemHandleRef::from_raw(fd, Core::HandleKind::File)));
}

ErrorOr<File> File::clone(Core::SystemHandleRef handle)
{
    return File(TRY(Core::System::dup(handle)));
}

// FIXME: IPC::Files transferred over the wire always set O_CLOEXEC during decoding. Perhaps we should add an option to
//        allow the receiver to decide whether to make it O_CLOEXEC or not. Or an attribute in the .ipc file?
ErrorOr<void> File::clear_close_on_exec()
{
    return Core::System::set_close_on_exec(m_handle, false);
}

template<>
ErrorOr<File> decode(Decoder& decoder)
{
    auto attachment = TRY(decoder.attachments().try_dequeue());
    auto handle = attachment.take_handle();
    if (!handle.is_valid())
        return Error::from_string_literal("Failed to obtain handle from attachment");
#ifndef AK_OS_WINDOWS
    TRY(Core::System::set_close_on_exec(handle, true));
#endif
    return File::adopt_handle(move(handle));
}

}

/*
 * Copyright (c) 2020, Sergey Bugaev <bugaevc@serenityos.org>
 * Copyright (c) 2021, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Noncopyable.h>
#include <AK/StdLibExtras.h>
#include <LibCore/Forward.h>
#include <LibCore/SystemHandle.h>

namespace IPC {

class File {
    AK_MAKE_NONCOPYABLE(File);

public:
    File() = default;

    static File adopt_file(NonnullOwnPtr<Core::File> file);
    static File adopt_handle(Core::SystemHandle);
    // NOTE: This tags the fd HandleKind::File; use adopt_handle() for anything that is not a plain file.
    static File adopt_fd(int fd);
    static ErrorOr<File> clone(Core::SystemHandleRef);

    File(File&& other) = default;
    File& operator=(File&& other) = default;

    ~File() = default;

    Core::SystemHandleRef handle() const { return m_handle.ref(); }
    int fd() const { return static_cast<int>(m_handle.raw_value()); }

    // These are 'const' since generated IPC messages expose all parameters by const reference.
    [[nodiscard]] Core::SystemHandle take_handle() const { return move(m_handle); }
    [[nodiscard]] int take_fd() const { return static_cast<int>(m_handle.leak().raw_value()); }

    ErrorOr<void> clear_close_on_exec();

private:
    explicit File(Core::SystemHandle handle)
        : m_handle(move(handle))
    {
    }

    mutable Core::SystemHandle m_handle;
};

}

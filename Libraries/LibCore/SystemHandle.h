/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Assertions.h>
#include <AK/Format.h>
#include <AK/HashFunctions.h>
#include <AK/Noncopyable.h>
#include <AK/Platform.h>
#include <AK/StdLibExtras.h>
#include <AK/StringView.h>
#include <AK/Traits.h>
#include <AK/Types.h>
#include <LibCore/Export.h>

namespace Core {

// The kind of native I/O object a handle refers to. On POSIX every kind is a plain file descriptor and the kind is
// advisory; on Windows the kind decides which API family operates on the value, so it must be assigned truthfully
// at creation time:
//
//  - File:   anything closed with CloseHandle() and read/written synchronously: disk files, console handles,
//            file-mapping handles (e.g. AnonymousBuffer), and inherited standard streams.
//  - Pipe:   the ends of System::create_pipe(). On Windows the read end is opened with FILE_FLAG_OVERLAPPED so the
//            event loop can wait for readability, which also means reads need an OVERLAPPED structure.
//  - Socket: a winsock SOCKET (closesocket()/WSA* family) or POSIX socket fd.
enum class HandleKind : u8 {
    Invalid,
    File,
    Pipe,
    Socket,
};

constexpr StringView handle_kind_to_string(HandleKind kind)
{
    switch (kind) {
    case HandleKind::Invalid:
        return "Invalid"sv;
    case HandleKind::File:
        return "File"sv;
    case HandleKind::Pipe:
        return "Pipe"sv;
    case HandleKind::Socket:
        return "Socket"sv;
    }
    VERIFY_NOT_REACHED();
}

// A non-owning, trivially copyable view of a native I/O handle and its kind.
class SystemHandleRef {
public:
#if defined(AK_OS_WINDOWS)
    // Large enough for both a kernel HANDLE and a winsock SOCKET.
    using NativeType = intptr_t;
#else
    using NativeType = int;
#endif

    constexpr SystemHandleRef() = default;

    static constexpr SystemHandleRef from_raw(NativeType value, HandleKind kind)
    {
        return SystemHandleRef { value, kind };
    }

#if defined(AK_OS_WINDOWS)
    static constexpr SystemHandleRef from_socket(NativeType socket)
    {
        return SystemHandleRef { socket, HandleKind::Socket };
    }

    static SystemHandleRef from_handle(void* handle, HandleKind kind)
    {
        VERIFY(kind == HandleKind::File || kind == HandleKind::Pipe);
        return SystemHandleRef { reinterpret_cast<NativeType>(handle), kind };
    }

    void* windows_handle() const
    {
        VERIFY(m_kind == HandleKind::File || m_kind == HandleKind::Pipe);
        return reinterpret_cast<void*>(m_value);
    }

    NativeType socket() const
    {
        VERIFY(m_kind == HandleKind::Socket);
        return m_value;
    }
#else
    static constexpr SystemHandleRef from_socket(int fd)
    {
        return SystemHandleRef { fd, HandleKind::Socket };
    }

    static constexpr SystemHandleRef from_fd(int fd, HandleKind kind = HandleKind::File)
    {
        return SystemHandleRef { fd, kind };
    }

    int fd() const
    {
        VERIFY(is_valid());
        return m_value;
    }
#endif

    constexpr HandleKind kind() const { return m_kind; }
    constexpr bool is_valid() const { return m_kind != HandleKind::Invalid; }
    constexpr NativeType raw_value() const { return m_value; }

    constexpr bool operator==(SystemHandleRef const&) const = default;

private:
    constexpr SystemHandleRef(NativeType value, HandleKind kind)
        : m_value(value)
        // A negative value is never a usable handle (it covers -1, INVALID_HANDLE_VALUE, and INVALID_SOCKET), so
        // normalize it to an invalid ref; callers commonly construct refs from possibly-unset fds.
        , m_kind(value < 0 ? HandleKind::Invalid : kind)
    {
    }

    NativeType m_value { -1 };
    HandleKind m_kind { HandleKind::Invalid };
};

// An owning native I/O handle: closes the underlying object on destruction, using the API family its kind demands.
class CORE_API SystemHandle {
    AK_MAKE_NONCOPYABLE(SystemHandle);

public:
    using NativeType = SystemHandleRef::NativeType;

    SystemHandle() = default;

    SystemHandle(SystemHandle&& other)
        : m_ref(exchange(other.m_ref, SystemHandleRef {}))
    {
    }

    SystemHandle& operator=(SystemHandle&& other)
    {
        if (this == &other)
            return *this;
        close();
        m_ref = exchange(other.m_ref, SystemHandleRef {});
        return *this;
    }

    ~SystemHandle()
    {
        close();
    }

    // Takes ownership of an already-tagged native handle.
    static SystemHandle adopt(SystemHandleRef ref)
    {
        return SystemHandle { ref };
    }

#if defined(AK_OS_WINDOWS)
    static SystemHandle adopt_socket(NativeType socket)
    {
        return SystemHandle { SystemHandleRef::from_socket(socket) };
    }

    static SystemHandle adopt_handle(void* handle, HandleKind kind)
    {
        return SystemHandle { SystemHandleRef::from_handle(handle, kind) };
    }

    void* windows_handle() const { return m_ref.windows_handle(); }
    NativeType socket() const { return m_ref.socket(); }
#else
    static SystemHandle adopt_fd(int fd, HandleKind kind = HandleKind::File)
    {
        return SystemHandle { SystemHandleRef::from_fd(fd, kind) };
    }

    int fd() const { return m_ref.fd(); }
#endif

    // Closes the underlying native object now; the handle becomes invalid.
    void close();

    // Releases ownership without closing.
    [[nodiscard]] SystemHandleRef leak() { return exchange(m_ref, SystemHandleRef {}); }

    SystemHandleRef ref() const { return m_ref; }
    operator SystemHandleRef() const { return m_ref; }

    HandleKind kind() const { return m_ref.kind(); }
    bool is_valid() const { return m_ref.is_valid(); }
    NativeType raw_value() const { return m_ref.raw_value(); }

private:
    explicit SystemHandle(SystemHandleRef ref)
        : m_ref(ref)
    {
    }

    SystemHandleRef m_ref;
};

}

namespace AK {

template<>
struct Formatter<Core::SystemHandleRef> : Formatter<FormatString> {
    ErrorOr<void> format(FormatBuilder& builder, Core::SystemHandleRef const& handle)
    {
#if defined(AK_OS_WINDOWS)
        return Formatter<FormatString>::format(builder, "{}({:#x})"sv, Core::handle_kind_to_string(handle.kind()), handle.raw_value());
#else
        return Formatter<FormatString>::format(builder, "{}({})"sv, Core::handle_kind_to_string(handle.kind()), handle.raw_value());
#endif
    }
};

template<>
struct Formatter<Core::SystemHandle> : Formatter<Core::SystemHandleRef> {
    ErrorOr<void> format(FormatBuilder& builder, Core::SystemHandle const& handle)
    {
        return Formatter<Core::SystemHandleRef>::format(builder, handle.ref());
    }
};

template<>
struct Traits<Core::SystemHandleRef> : DefaultTraits<Core::SystemHandleRef> {
    static unsigned hash(Core::SystemHandleRef const& handle)
    {
        return pair_int_hash(u64_hash(static_cast<u64>(handle.raw_value())), to_underlying(handle.kind()));
    }
};

}

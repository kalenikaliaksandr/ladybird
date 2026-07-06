/*
 * Copyright (c) 2021, Andreas Kling <andreas@ladybird.org>
 * Copyright (c) 2022-2023, MacDue <macdue@dueutil.tech>
 * Copyright (c) 2023-2024, Sam Atkins <atkinssj@serenityos.org>
 * Copyright (c) 2024, Tim Flynn <trflynn89@serenityos.org>
 * Copyright (c) 2024, stasoid <stasoid@yahoo.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Assertions.h>
#include <AK/ScopeGuard.h>
#include <AK/String.h>
#include <AK/Utf16View.h>
#include <AK/Vector.h>
#include <AK/Windows.h>
#include <LibCore/Process.h>
#include <LibCore/System.h>

namespace Core {

// A job object configured to kill all assigned processes when its last handle is closed. This process holds the only
// handle, so all processes spawned with die_with_parent are terminated by the kernel when this process exits for any
// reason, including a crash.
static HANDLE kill_children_on_exit_job()
{
    static HANDLE job = []() -> HANDLE {
        HANDLE job = CreateJobObjectW(nullptr, nullptr);
        if (!job)
            return nullptr;

        JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits = {};
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits, sizeof(limits))) {
            CloseHandle(job);
            return nullptr;
        }

        return job;
    }();
    return job;
}

Process::Process(Process&& other)
    : m_handle(exchange(other.m_handle, nullptr))
{
}

Process& Process::operator=(Process&& other)
{
    m_handle = exchange(other.m_handle, nullptr);
    return *this;
}

Process::~Process()
{
    if (m_handle)
        CloseHandle(m_handle);
}

Process Process::current()
{
    return GetCurrentProcess();
}

ErrorOr<Process> Process::spawn(ProcessSpawnOptions const& options)
{
    StringBuilder builder;
    if (!options.search_for_executable_in_path && !options.executable.find_any_of("\\/:"sv).has_value())
        builder.appendff("\"./{}\" ", options.executable);
    else
        builder.appendff("\"{}\" ", options.executable);

    for (auto arg : options.arguments)
        builder.appendff("\"{}\" ", arg);

    builder.append('\0');
    ByteBuffer command_line = TRY(builder.to_byte_buffer());

    STARTUPINFO startup_info = {};
    startup_info.cb = sizeof(startup_info);
    PROCESS_INFORMATION process_info = {};

    // Handles we open for FileAction::OpenFile; the child inherits its own copies, so ours are closed on return.
    Vector<HANDLE> opened_handles;
    ScopeGuard close_opened_handles = [&] {
        for (auto handle : opened_handles)
            CloseHandle(handle);
    };

    auto redirect_std_handle = [&](FileAction::StandardStream stream, HANDLE handle) -> ErrorOr<void> {
        // The child inherits handles by value, so the handle must be marked inheritable in this process.
        if (!SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT))
            return Error::from_windows_error();

        switch (stream) {
        case FileAction::StandardStream::Input:
            startup_info.hStdInput = handle;
            break;
        case FileAction::StandardStream::Output:
            startup_info.hStdOutput = handle;
            break;
        case FileAction::StandardStream::Error:
            startup_info.hStdError = handle;
            break;
        }

        startup_info.dwFlags |= STARTF_USESTDHANDLES;
        return {};
    };

    for (auto const& file_action : options.file_actions) {
        TRY(file_action.visit(
            [&](FileAction::OpenFile const& action) -> ErrorOr<void> {
                auto handle = to_handle(TRY(System::open(action.path.view(), File::open_mode_to_options(action.mode), action.permissions)));
                opened_handles.append(handle);
                return redirect_std_handle(action.stream, handle);
            },
            [&](FileAction::CloseFile const&) -> ErrorOr<void> {
                // On POSIX, CloseFile closes the child's inherited copy of a pipe fd that was dup2()'d onto a standard
                // stream. On Windows, inheritance hands the child exactly one copy of each handle, so there is no
                // extra descriptor to close.
                return {};
            },
            [&](FileAction::DupFd const& action) -> ErrorOr<void> {
                return redirect_std_handle(action.stream, action.source.windows_handle());
            }));
    }

    if (startup_info.dwFlags & STARTF_USESTDHANDLES) {
        // Any standard stream not covered by a file action still points at ours.
        if (!startup_info.hStdInput)
            startup_info.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
        if (!startup_info.hStdOutput)
            startup_info.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
        if (!startup_info.hStdError)
            startup_info.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    }

    // Start the process suspended so it can be placed into the kill-on-close job before it runs (and before it can
    // spawn children of its own, which inherit job membership).
    DWORD creation_flags = options.die_with_parent ? CREATE_SUSPENDED : 0;

    BOOL result = CreateProcess(
        NULL,
        (char*)command_line.data(),
        NULL, // process security attributes
        NULL, // primary thread security attributes
        TRUE, // handles are inherited
        creation_flags,
        NULL, // use parent's environment
        NULL, // working directory
        &startup_info,
        &process_info);

    if (!result)
        return Error::from_windows_error();

    if (options.die_with_parent) {
        if (auto job = kill_children_on_exit_job(); job)
            AssignProcessToJobObject(job, process_info.hProcess);
        ResumeThread(process_info.hThread);
    }

    CloseHandle(process_info.hThread);

    return Process(process_info.hProcess);
}

ErrorOr<Process> Process::spawn(StringView path, ReadonlySpan<ByteString> arguments)
{
    return spawn({
        .executable = path,
        .arguments = Vector<ByteString> { arguments },
    });
}

ErrorOr<Process> Process::spawn(StringView path, ReadonlySpan<StringView> arguments)
{
    Vector<ByteString> backing_strings;
    backing_strings.ensure_capacity(arguments.size());
    for (auto argument : arguments)
        backing_strings.append(argument);

    return spawn({
        .executable = path,
        .arguments = backing_strings,
    });
}

ErrorOr<void> Process::terminate()
{
    if (!TerminateProcess(m_handle, 1))
        return Error::from_windows_error();
    return {};
}

void Process::terminate_immediately(int status)
{
    // TerminateProcess() is stronger than the CRT's _exit(), since it skips
    // DLL detach notifications and the other ExitProcess() teardown paths.
    TerminateProcess(GetCurrentProcess(), static_cast<UINT>(status));
    VERIFY_NOT_REACHED();
}

// Get the full path of the executable file of the current process
ErrorOr<String> Process::get_name()
{
    Vector<wchar_t, MAX_PATH> path;
    path.resize(MAX_PATH);

    DWORD length = GetModuleFileNameW(NULL, path.data(), MAX_PATH);

    if (length == path.size() && GetLastError() == ERROR_INSUFFICIENT_BUFFER) {
        path.resize(UNICODE_STRING_MAX_CHARS);
        length = GetModuleFileNameW(NULL, path.data(), UNICODE_STRING_MAX_CHARS);
    }

    if (!length)
        return Error::from_windows_error();

    return MUST(Utf16View { reinterpret_cast<char16_t const*>(path.data()), length }.to_utf8());
}

ErrorOr<bool> Process::is_being_debugged()
{
    return IsDebuggerPresent();
}

// Forces the process to sleep until a debugger is attached, then breaks.
void Process::wait_for_debugger_and_break()
{
    bool print_message = true;
    for (;;) {
        if (IsDebuggerPresent()) {
            DebugBreak();
            return;
        }
        if (print_message) {
            dbgln("Process {} with pid {} is sleeping, waiting for debugger.", Process::get_name(), GetCurrentProcessId());
            print_message = false;
        }
        Sleep(100);
    }
}

pid_t Process::pid() const
{
    return GetProcessId(m_handle);
}

ErrorOr<int> Process::wait_for_termination() const
{
    auto result = WaitForSingleObject(m_handle, INFINITE);
    if (result == WAIT_FAILED)
        return Error::from_windows_error();

    DWORD exit_code = 0;
    if (!GetExitCodeProcess(m_handle, &exit_code))
        return Error::from_windows_error();

    return exit_code;
}

}

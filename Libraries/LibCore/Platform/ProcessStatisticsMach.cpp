/*
 * Copyright (c) 2024, Andrew Kaster <akaster@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Platform.h>

#if !defined(AK_OS_MACH)
#    error "This file is only available on Mach platforms"
#endif

#include <AK/ByteString.h>
#include <AK/Time.h>
#include <LibCore/MachPort.h>
#include <LibCore/Platform/MachMessageTypes.h>
#include <LibCore/Platform/ProcessStatisticsMach.h>

namespace Core::Platform {

static auto user_hz = sysconf(_SC_CLK_TCK);

ErrorOr<void> update_process_statistics(ProcessStatistics& statistics)
{
    host_cpu_load_info_data_t cpu_info {};
    mach_msg_type_number_t count = HOST_CPU_LOAD_INFO_COUNT;
    auto res = host_statistics(mach_host_self(), HOST_CPU_LOAD_INFO, reinterpret_cast<host_info_t>(&cpu_info), &count);
    if (res != KERN_SUCCESS) {
        dbgln("Failed to get host statistics: {}", mach_error_string(res));
        return Core::mach_error_to_error(res);
    }

    u64 total_cpu_ticks = 0;
    total_cpu_ticks += cpu_info.cpu_ticks[CPU_STATE_USER];
    total_cpu_ticks += cpu_info.cpu_ticks[CPU_STATE_SYSTEM];
    total_cpu_ticks += cpu_info.cpu_ticks[CPU_STATE_NICE];
    total_cpu_ticks += cpu_info.cpu_ticks[CPU_STATE_IDLE];

    auto const total_cpu_ticks_diff = total_cpu_ticks - statistics.total_time_scheduled;
    auto const total_cpu_seconds_diff = total_cpu_ticks_diff / (static_cast<float>(user_hz));
    auto const total_cpu_micro_diff = total_cpu_seconds_diff * 1'000'000;
    statistics.total_time_scheduled = total_cpu_ticks;

    for (auto& process : statistics.processes) {
        mach_task_basic_info_data_t basic_info {};
        count = MACH_TASK_BASIC_INFO_COUNT;
        res = task_info(process->child_task_port.port(), MACH_TASK_BASIC_INFO, reinterpret_cast<task_info_t>(&basic_info), &count);
        if (res != KERN_SUCCESS) {
            dbgln("Failed to get task info for pid {}: {}", process->pid, mach_error_string(res));
            return Core::mach_error_to_error(res);
        }

        process->memory_usage_bytes = basic_info.resident_size;

        task_thread_times_info_data_t time_info {};
        count = TASK_THREAD_TIMES_INFO_COUNT;
        res = task_info(process->child_task_port.port(), TASK_THREAD_TIMES_INFO, reinterpret_cast<task_info_t>(&time_info), &count);
        if (res != KERN_SUCCESS) {
            dbgln("Failed to get thread times info for pid {}: {}", process->pid, mach_error_string(res));
            return Core::mach_error_to_error(res);
        }

        timeval scratch_timeval = { static_cast<time_t>(time_info.user_time.seconds), static_cast<suseconds_t>(time_info.user_time.microseconds) };
        auto time_in_process = AK::Duration::from_timeval(scratch_timeval);
        scratch_timeval = { static_cast<time_t>(time_info.system_time.seconds), static_cast<suseconds_t>(time_info.system_time.microseconds) };
        time_in_process += AK::Duration::from_timeval(scratch_timeval);

        auto time_diff_process = time_in_process - AK::Duration::from_microseconds(process->time_spent_in_process);
        process->time_spent_in_process = time_in_process.to_microseconds();

        process->cpu_percent = 0.0f;
        if (time_diff_process > AK::Duration::zero())
            process->cpu_percent = 100.0f * static_cast<float>(time_diff_process.to_microseconds()) / total_cpu_micro_diff;
    }

    return {};
}

MachServerRegistration register_with_mach_server(ByteString const& server_name)
{
    auto server_port_or_error = Core::MachPort::look_up_from_bootstrap_server(server_name);
    if (server_port_or_error.is_error()) {
        dbgln("Failed to lookup server port: {}", server_port_or_error.error());
        VERIFY_NOT_REACHED();
    }
    auto server_port = server_port_or_error.release_value();

    // Create a temporary reply port so the server can send IPC channel ports back
    auto reply_port = MUST(Core::MachPort::create_with_right(Core::MachPort::PortRight::Receive));

    // Send our own task port to the server so they can query statistics about us
    // Also include reply port so server can send IPC channel ports back
    MessageWithSelfTaskPort message {};
    message.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND_ONCE) | MACH_MSGH_BITS_COMPLEX;
    message.header.msgh_size = sizeof(message);
    message.header.msgh_remote_port = server_port.port();
    message.header.msgh_local_port = reply_port.port();
    message.header.msgh_id = SELF_TASK_PORT_MESSAGE_ID;
    message.body.msgh_descriptor_count = 1;
    message.port_descriptor.name = mach_task_self();
    message.port_descriptor.disposition = MACH_MSG_TYPE_COPY_SEND;
    message.port_descriptor.type = MACH_MSG_PORT_DESCRIPTOR;

    mach_msg_timeout_t const timeout = 100; // milliseconds

    auto const send_result = mach_msg(&message.header, MACH_SEND_MSG | MACH_SEND_TIMEOUT, message.header.msgh_size, 0, MACH_PORT_NULL, timeout, MACH_PORT_NULL);
    if (send_result != KERN_SUCCESS) {
        dbgln("Failed to send message to server: {}", mach_error_string(send_result));
        VERIFY_NOT_REACHED();
    }

    // Wait for reply with IPC channel ports
    ReceivedIPCChannelPortsMessage reply {};
    mach_msg_timeout_t const reply_timeout = 5000; // 5 seconds

    auto const recv_result = mach_msg(&reply.header, MACH_RCV_MSG | MACH_RCV_TIMEOUT, 0, sizeof(reply),
        reply_port.port(), reply_timeout, MACH_PORT_NULL);
    if (recv_result != KERN_SUCCESS) {
        dbgln("Failed to receive IPC channel ports: {}", mach_error_string(recv_result));
        VERIFY_NOT_REACHED();
    }

    if (reply.header.msgh_id != IPC_CHANNEL_PORTS_MESSAGE_ID) {
        dbgln("Received unexpected reply message id: {}", reply.header.msgh_id);
        VERIFY_NOT_REACHED();
    }

    auto ipc_receive_right = Core::MachPort::adopt_right(reply.receive_port.name, Core::MachPort::PortRight::Receive);
    auto ipc_send_right = Core::MachPort::adopt_right(reply.send_port.name, Core::MachPort::PortRight::Send);

    return MachServerRegistration {
        move(server_port),
        move(ipc_receive_right),
        move(ipc_send_right),
    };
}

}

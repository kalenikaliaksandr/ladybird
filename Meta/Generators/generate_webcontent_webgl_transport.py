#!/usr/bin/env python3

# Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
#
# SPDX-License-Identifier: BSD-2-Clause

import argparse
import sys

from pathlib import Path
from typing import TextIO

sys.path.append(str(Path(__file__).resolve().parent))

from libweb_webgl import cpp_sender_type
from libweb_webgl import ipc_message_name
from libweb_webgl import ipc_message_params
from libweb_webgl import ipc_response_params
from libweb_webgl import is_ipc_command
from libweb_webgl import is_ipc_sync
from libweb_webgl import load_functions

# Generates WebContent's RemoteWebGLTransport implementation: each virtual is a one-line
# forward to the generated IPC senders on CompositorConnection. Async sends are dropped
# when the compositor is gone; failed synchronous sends mark the compositor lost and
# leave outputs zeroed (which the proxy translates into lost-context semantics).


def pascal_case(message_name: str) -> str:
    return "".join(part.capitalize() for part in message_name.split("_"))


def is_container(ipc_type: str) -> bool:
    return ipc_type.startswith("Vector<") or ipc_type in ("ByteBuffer", "String")


def transport_signature(function: dict, qualifier: str = "") -> tuple:
    if is_ipc_sync(function):
        parameters = [f"{ipc_type} {name}" for ipc_type, name, _ in [("u64", "webgl_context_id", None)] + ipc_message_params(function)]
    else:
        parameters = [f"{cpp_sender_type(ipc_type)} {name}" for ipc_type, name, _ in [("u64", "webgl_context_id", None)] + ipc_message_params(function)]
    return_type = "void"
    responses = ipc_response_params(function) if is_ipc_sync(function) else []
    returns_first = bool(responses) and (len(responses) == 1 or responses[0][1] == "return_value")
    reference_responses = responses
    if returns_first:
        return_type = responses[0][0]
        reference_responses = responses[1:]
    for ipc_type, name, _ in reference_responses:
        parameters.append(f"{ipc_type}& {name}")
    signature = f"{return_type} {qualifier}{ipc_message_name(function)}({', '.join(parameters)})"
    return signature, returns_first, responses


def write_header_file(out: TextIO, functions: list) -> None:
    out.write("""#pragma once

#include <LibWeb/WebGL/RemoteWebGLTransport.h>
#include <WebContent/CompositorConnection.h>

namespace WebContent {

// Holds a strong reference to the connection it was created against, so a WebGL context
// keeps a stable (if dead, after a compositor crash) channel for its whole lifetime.
class WebContentRemoteWebGLTransport final : public Web::WebGL::RemoteWebGLTransport {
public:
    explicit WebContentRemoteWebGLTransport(NonnullRefPtr<CompositorConnection>);

private:
    virtual CreateResult create_context(u64 webgl_context_id, u32 webgl_version, bool depth, bool stencil, bool antialias) override;
    virtual void destroy_context(u64 webgl_context_id) override;
    virtual Gfx::ShareableBitmap read_back_drawing_buffer(u64 webgl_context_id) override;

""")
    for function in functions:
        if is_ipc_command(function) or is_ipc_sync(function):
            signature, _, _ = transport_signature(function)
            out.write(f"    virtual {signature} override;\n")
    out.write("""
    NonnullRefPtr<CompositorConnection> m_connection;
};

}
""")


def write_implementation_file(out: TextIO, functions: list) -> None:
    out.write("""#include <WebContent/WebContentRemoteWebGLTransport.h>

namespace WebContent {

WebContentRemoteWebGLTransport::WebContentRemoteWebGLTransport(NonnullRefPtr<CompositorConnection> connection)
    : m_connection(move(connection))
{
}

WebContentRemoteWebGLTransport::CreateResult WebContentRemoteWebGLTransport::create_context(u64 webgl_context_id, u32 webgl_version, bool depth, bool stencil, bool antialias)
{
    if (!m_connection->can_send_to_compositor())
        return {};
    auto response = m_connection->try_create_webgl_context(webgl_context_id, webgl_version, depth, stencil, antialias);
    if (response.is_error()) {
        m_connection->notify_compositor_lost();
        return {};
    }
    return { response.value().success(), response.value().take_supported_extensions() };
}

void WebContentRemoteWebGLTransport::destroy_context(u64 webgl_context_id)
{
    if (!m_connection->can_send_to_compositor())
        return;
    m_connection->async_destroy_webgl_context(webgl_context_id);
}

Gfx::ShareableBitmap WebContentRemoteWebGLTransport::read_back_drawing_buffer(u64 webgl_context_id)
{
    if (!m_connection->can_send_to_compositor())
        return {};
    auto response = m_connection->try_get_webgl_drawing_buffer(webgl_context_id);
    if (response.is_error()) {
        m_connection->notify_compositor_lost();
        return {};
    }
    return response.release_value();
}
""")
    for function in functions:
        command = is_ipc_command(function)
        sync = is_ipc_sync(function)
        if not command and not sync:
            continue
        signature, returns_first, responses = transport_signature(function, "WebContentRemoteWebGLTransport::")
        message = ipc_message_name(function)
        sync_arguments = ", ".join(f"move({name})" if is_container(ipc_type) else name for ipc_type, name, _ in [("u64", "webgl_context_id", None)] + ipc_message_params(function))
        arguments = ", ".join(name for _, name, _ in [("u64", "webgl_context_id", None)] + ipc_message_params(function))
        out.write(f"\n{signature}\n{{\n")
        if command:
            out.write("    if (!m_connection->can_send_to_compositor())\n        return;\n")
            out.write(f"    m_connection->async_{message}({arguments});\n")
            out.write("}\n")
            continue
        failure = "return {};" if returns_first else "return;"
        out.write(f"    if (!m_connection->can_send_to_compositor())\n        {failure}\n")
        out.write(f"    auto response = m_connection->try_{message}({sync_arguments});\n")
        out.write("    if (response.is_error()) {\n")
        out.write("        m_connection->notify_compositor_lost();\n")
        out.write(f"        {failure}\n")
        out.write("    }\n")
        if len(responses) == 1:
            # Single-output synchronous senders unwrap straight to the value.
            out.write("    return response.release_value();\n")
            out.write("}\n")
            continue
        reference_responses = responses[1:] if returns_first else responses
        for ipc_type, name, _ in reference_responses:
            accessor = f"take_{name}" if is_container(ipc_type) else name
            out.write(f"    {name} = response.value().{accessor}();\n")
        if returns_first:
            out.write(f"    return response.value().return_value();\n")
        out.write("}\n")
    out.write("\n}\n")


def main():
    parser = argparse.ArgumentParser(description="Generate WebContent's remote WebGL transport", add_help=False)
    parser.add_argument("--help", action="help", help="Show this help message and exit")
    parser.add_argument("-h", "--header", required=True, help="Path to the header file to generate")
    parser.add_argument("-c", "--implementation", required=True, help="Path to the implementation file to generate")
    parser.add_argument("-j", "--json", required=True, help="Path to the JSON file to read from")
    args = parser.parse_args()

    functions = load_functions(args.json)

    with open(args.header, "w", encoding="utf-8") as output_file:
        write_header_file(output_file, functions)

    with open(args.implementation, "w", encoding="utf-8") as output_file:
        write_implementation_file(output_file, functions)


if __name__ == "__main__":
    main()

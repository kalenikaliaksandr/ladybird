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

# Generates Web::WebGL::RemoteWebGLTransport: WebContent's channel to the remote WebGL
# host, with one pure virtual per WebGL message in sender wire shape (spans and views go
# zero-copy into the IPC encoder). LibWeb must not see the Compositor endpoint header,
# so this abstraction is what the generated proxy calls; WebContent implements it over
# the generated IPC senders.
#
# Synchronous calls return their single response value directly when there is exactly
# one (or a leading return_value); additional outputs come back through references,
# left zero-initialized when the compositor is gone.


def transport_signature(function: dict, qualifier: str = "") -> str:
    # Generated async senders take wire types (spans go zero-copy into the encoder);
    # synchronous senders take the stored types, so the sync methods do too.
    if is_ipc_sync(function):
        parameters = [f"{ipc_type} {name}" for ipc_type, name, _ in [("u64", "webgl_context_id", None)] + ipc_message_params(function)]
    else:
        parameters = [f"{cpp_sender_type(ipc_type)} {name}" for ipc_type, name, _ in [("u64", "webgl_context_id", None)] + ipc_message_params(function)]
    return_type = "void"
    if is_ipc_sync(function):
        responses = ipc_response_params(function)
        if len(responses) == 1 or (responses and responses[0][1] == "return_value"):
            return_type = responses[0][0]
            responses = responses[1:]
        for ipc_type, name, _ in responses:
            parameters.append(f"{ipc_type}& {name}")
    return f"{return_type} {qualifier}{ipc_message_name(function)}({', '.join(parameters)})"


def write_header_file(out: TextIO, functions: list) -> None:
    out.write("""#pragma once

#include <AK/ByteBuffer.h>
#include <AK/RefCounted.h>
#include <AK/Span.h>
#include <AK/String.h>
#include <AK/Vector.h>
#include <LibGfx/ShareableBitmap.h>
#include <LibWeb/Export.h>

namespace Web::WebGL {

// WebContent's channel to a remote WebGL host in the Compositor process. RefCounted and
// bound to one compositor connection: a context created against one connection keeps a
// stable channel for its whole lifetime, and if the compositor dies the channel simply
// goes dead (sends are dropped, sync calls return zeroed values) - the context is lost.
class WEB_API RemoteWebGLTransport : public AK::RefCounted<RemoteWebGLTransport> {
public:
    virtual ~RemoteWebGLTransport();

    struct CreateResult {
        bool success { false };
        Vector<String> supported_extensions;
    };
    virtual CreateResult create_context(u64 webgl_context_id, u32 webgl_version, bool depth, bool stencil, bool antialias) = 0;
    virtual void destroy_context(u64 webgl_context_id) = 0;

    // Synchronously reads back the live drawing buffer; pixel data travels as shared
    // memory rather than message bytes.
    virtual Gfx::ShareableBitmap read_back_drawing_buffer(u64 webgl_context_id) = 0;

    // One method per WebGL message.
""")
    for function in functions:
        if is_ipc_command(function) or is_ipc_sync(function):
            out.write(f"    virtual {transport_signature(function)} = 0;\n")
    out.write("""};

}
""")


def write_implementation_file(out: TextIO, functions: list) -> None:
    _ = functions
    out.write("""#include <LibWeb/WebGL/RemoteWebGLTransport.h>

namespace Web::WebGL {

RemoteWebGLTransport::~RemoteWebGLTransport() = default;

}
""")


def main():
    parser = argparse.ArgumentParser(description="Generate the remote WebGL transport interface", add_help=False)
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

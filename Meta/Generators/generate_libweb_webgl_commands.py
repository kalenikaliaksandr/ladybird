#!/usr/bin/env python3

# Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
#
# SPDX-License-Identifier: BSD-2-Clause

import argparse
import sys

from pathlib import Path
from typing import TextIO

sys.path.append(str(Path(__file__).resolve().parent))

from libweb_webgl import command_name
from libweb_webgl import command_struct_fields
from libweb_webgl import load_functions
from libweb_webgl import sync_reply_fields
from libweb_webgl import sync_request_fields

# Generates the WebGL command stream vocabulary from GLFunctions.json: one opcode and one
# trivially-copyable struct per "command"/"gen" entry. The recorder (WebContent) and the
# replayer (Compositor) are generated from the same source of truth, so the wire format
# cannot drift between the two sides.


def write_header_file(out: TextIO, functions: list) -> None:
    commands = [f for f in functions if f["category"] in ("command", "gen")]

    out.write("""#pragma once

#include <AK/Assertions.h>
#include <AK/StdLibExtras.h>
#include <AK/StringView.h>
#include <AK/Types.h>
#include <LibWeb/Export.h>
#include <LibWeb/WebGL/GLFunctions.h>

namespace Web::WebGL {

#define ENUMERATE_WEBGL_COMMANDS(V) \\
""")
    for f in commands:
        out.write(f"    V({command_name(f)}) \\\n")
    out.write("""
enum class WebGLCommandType : u16 {
#define __ENUMERATE_WEBGL_COMMAND(name) name,
    ENUMERATE_WEBGL_COMMANDS(__ENUMERATE_WEBGL_COMMAND)
#undef __ENUMERATE_WEBGL_COMMAND
};

""")
    out.write(f"inline constexpr u16 webgl_command_type_count = {len(commands)};\n")
    out.write("""
WEB_API StringView to_string(WebGLCommandType);

namespace Commands {
""")
    for f in commands:
        name = command_name(f)
        out.write(f"\nstruct {name} {{\n")
        out.write(f"    static constexpr auto command_type = WebGLCommandType::{name};\n")
        for cpp_type, field_name, _ in command_struct_fields(f):
            out.write(f"    {cpp_type} {field_name} {{}};\n")
        out.write("};\n")
    out.write("""
}

template<typename Callback>
decltype(auto) visit_webgl_command_type(WebGLCommandType type, Callback&& callback)
{
    switch (type) {
#define __ENUMERATE_WEBGL_COMMAND(name) \\
    case WebGLCommandType::name:        \\
        return callback.template operator()<Commands::name>();
        ENUMERATE_WEBGL_COMMANDS(__ENUMERATE_WEBGL_COMMAND)
#undef __ENUMERATE_WEBGL_COMMAND
    }
    VERIFY_NOT_REACHED();
}

#define __ENUMERATE_WEBGL_COMMAND(name) static_assert(IsTriviallyCopyable<Commands::name>);
ENUMERATE_WEBGL_COMMANDS(__ENUMERATE_WEBGL_COMMAND)
#undef __ENUMERATE_WEBGL_COMMAND

""")

    sync_calls = [f for f in functions if f["category"] == "sync"]
    out.write("#define ENUMERATE_WEBGL_SYNC_CALLS(V) \\\n")
    for f in sync_calls:
        out.write(f"    V({command_name(f)}) \\\n")
    out.write("""
enum class WebGLSyncCallType : u16 {
#define __ENUMERATE_WEBGL_SYNC_CALL(name) name,
    ENUMERATE_WEBGL_SYNC_CALLS(__ENUMERATE_WEBGL_SYNC_CALL)
#undef __ENUMERATE_WEBGL_SYNC_CALL
};

""")
    out.write(f"inline constexpr u16 webgl_sync_call_type_count = {len(sync_calls)};\n")
    out.write("""
WEB_API StringView to_string(WebGLSyncCallType);

namespace SyncCalls {
""")
    for f in sync_calls:
        name = command_name(f)
        out.write(f"\nstruct {name} {{\n")
        out.write(f"    static constexpr auto call_type = WebGLSyncCallType::{name};\n")
        out.write("    struct Request {\n")
        for cpp_type, field_name, _ in sync_request_fields(f):
            out.write(f"        {cpp_type} {field_name} {{}};\n")
        out.write("    };\n")
        out.write("    struct Reply {\n")
        for cpp_type, field_name, _ in sync_reply_fields(f):
            out.write(f"        {cpp_type} {field_name} {{}};\n")
        out.write("    };\n")
        out.write("};\n")
    out.write("""
}

template<typename Callback>
decltype(auto) visit_webgl_sync_call_type(WebGLSyncCallType type, Callback&& callback)
{
    switch (type) {
#define __ENUMERATE_WEBGL_SYNC_CALL(name) \\
    case WebGLSyncCallType::name:         \\
        return callback.template operator()<SyncCalls::name>();
        ENUMERATE_WEBGL_SYNC_CALLS(__ENUMERATE_WEBGL_SYNC_CALL)
#undef __ENUMERATE_WEBGL_SYNC_CALL
    }
    VERIFY_NOT_REACHED();
}

#define __ENUMERATE_WEBGL_SYNC_CALL(name)                       \\
    static_assert(IsTriviallyCopyable<SyncCalls::name::Request>); \\
    static_assert(IsTriviallyCopyable<SyncCalls::name::Reply>);
ENUMERATE_WEBGL_SYNC_CALLS(__ENUMERATE_WEBGL_SYNC_CALL)
#undef __ENUMERATE_WEBGL_SYNC_CALL

}
""")


def write_implementation_file(out: TextIO, functions: list) -> None:
    out.write("""#include <LibWeb/WebGL/WebGLCommands.h>

namespace Web::WebGL {

StringView to_string(WebGLCommandType type)
{
    switch (type) {
#define __ENUMERATE_WEBGL_COMMAND(name) \\
    case WebGLCommandType::name:        \\
        return #name##sv;
        ENUMERATE_WEBGL_COMMANDS(__ENUMERATE_WEBGL_COMMAND)
#undef __ENUMERATE_WEBGL_COMMAND
    }
    VERIFY_NOT_REACHED();
}

StringView to_string(WebGLSyncCallType type)
{
    switch (type) {
#define __ENUMERATE_WEBGL_SYNC_CALL(name) \\
    case WebGLSyncCallType::name:         \\
        return #name##sv;
        ENUMERATE_WEBGL_SYNC_CALLS(__ENUMERATE_WEBGL_SYNC_CALL)
#undef __ENUMERATE_WEBGL_SYNC_CALL
    }
    VERIFY_NOT_REACHED();
}

}
""")


def main():
    parser = argparse.ArgumentParser(description="Generate WebGL command stream vocabulary", add_help=False)
    parser.add_argument("--help", action="help", help="Show this help message and exit")
    parser.add_argument("-h", "--header", required=True, help="Path to the WebGLCommands header file to generate")
    parser.add_argument(
        "-c", "--implementation", required=True, help="Path to the WebGLCommands implementation file to generate"
    )
    parser.add_argument("-j", "--json", required=True, help="Path to the JSON file to read from")
    args = parser.parse_args()

    functions = load_functions(args.json)

    with open(args.header, "w", encoding="utf-8") as output_file:
        write_header_file(output_file, functions)

    with open(args.implementation, "w", encoding="utf-8") as output_file:
        write_implementation_file(output_file, functions)


if __name__ == "__main__":
    main()

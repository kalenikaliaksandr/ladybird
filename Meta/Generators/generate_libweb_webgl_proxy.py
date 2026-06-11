#!/usr/bin/env python3

# Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
#
# SPDX-License-Identifier: BSD-2-Clause

import argparse
import sys

from io import StringIO
from pathlib import Path
from typing import TextIO

sys.path.append(str(Path(__file__).resolve().parent))

from libweb_webgl import gl_element_type
from libweb_webgl import ipc_message_name
from libweb_webgl import ipc_message_params
from libweb_webgl import ipc_parameter_name
from libweb_webgl import ipc_response_params
from libweb_webgl import is_const_pointer
from libweb_webgl import is_ipc_sync
from libweb_webgl import is_pointer
from libweb_webgl import load_functions
from libweb_webgl import snake_case

# Generates WebGLContextProxy, WebContent's drop-in replacement for the GL seam: the
# same method signatures as the generated GLFunctions class, but each call becomes one
# IPC message through RemoteWebGLTransport. Pointer arguments turn into spans using the
# JSON's byte-size expressions (method parameter names match the GL argument names, so
# the expressions are used verbatim); object creation hands out client-side ids; sync
# calls copy reply data back into the caller's buffers, never more than asked for.


def method_signature(function: dict, qualifier: str = "") -> str:
    args = ", ".join(f"{arg['type']} {arg['name']}" for arg in function["args"])
    return f"{function['return']} {qualifier}{function['method']}({args})"


def element_count_expression(arg: dict) -> str:
    element = gl_element_type(arg["type"])
    if element == "void":
        return f"static_cast<size_t>({arg['payload']})"
    return f"static_cast<size_t>({arg['payload']}) / sizeof({element})"


def span_type(arg: dict) -> str:
    element = gl_element_type(arg["type"])
    if element == "void":
        return "ReadonlyBytes"
    cpp_element = {"GLfloat": "float", "GLint": "i32", "GLuint": "u32", "GLenum": "u32", "GLboolean": "u8"}[element]
    return f"ReadonlySpan<{cpp_element}>"


def span_data_expression(arg: dict) -> str:
    element = gl_element_type(arg["type"])
    if element == "void":
        return f"static_cast<u8 const*>({arg['name']})"
    cpp_element = {"GLfloat": "float const*", "GLint": "i32 const*", "GLuint": "u32 const*", "GLenum": "u32 const*", "GLboolean": "u8 const*"}[element]
    return f"reinterpret_cast<{cpp_element}>({arg['name']})"


# Emits span/argument conversion for one in-argument and returns the transport call
# argument expression(s).
def emit_command_argument(out: TextIO, function: dict, arg: dict, message_param_name: str) -> list:
    name = arg["name"]
    if arg.get("object") and not is_pointer(arg):
        if arg["type"] == "GLsync":
            return [f"static_cast<WebGLObjectId>(reinterpret_cast<uintptr_t>({name}))"]
        return [name]
    if arg.get("object") and is_const_pointer(arg):
        out.write(f"    ReadonlySpan<u32> {message_param_name}_span {{ {name}, {element_count_expression(arg)} }};\n")
        return [f"{message_param_name}_span"]
    if arg.get("string"):
        return [f"StringView {{ {name}, __builtin_strlen({name}) }}"]
    if arg.get("offset"):
        return [f"static_cast<i64>(reinterpret_cast<uintptr_t>({name}))"]
    if "payload" in arg:
        if arg.get("nullable"):
            out.write(f"    {span_type(arg)} {message_param_name}_span {{ {name} ? {span_data_expression(arg)} : nullptr, {name} ? {element_count_expression(arg)} : 0 }};\n")
            return [f"{name} != nullptr", f"{message_param_name}_span"]
        out.write(f"    {span_type(arg)} {message_param_name}_span {{ {span_data_expression(arg)}, {element_count_expression(arg)} }};\n")
        return [f"{message_param_name}_span"]
    return [name]


# Synchronous senders take the stored IPC types (Vector<T>/String) by value rather than
# the wire spans async senders use, so sync in-arguments are copied into owning values.
# Sync calls are cold paths; the extra copy does not matter.
def emit_sync_argument(out: TextIO, function: dict, arg: dict, message_param_name: str) -> list:
    name = arg["name"]
    if arg.get("string"):
        return [f"String::from_utf8_with_replacement_character(StringView {{ {name}, __builtin_strlen({name}) }})"]
    if arg.get("object") and is_const_pointer(arg):
        out.write(f"    Vector<u32> {message_param_name}_value;\n")
        out.write(f"    {message_param_name}_value.append({name}, {element_count_expression(arg)});\n")
        return [f"move({message_param_name}_value)"]
    if "payload" in arg:
        element = gl_element_type(arg["type"])
        if element == "void":
            out.write(f"    ByteBuffer {message_param_name}_value;\n")
            out.write(f"    if ({name})\n        {message_param_name}_value = MUST(ByteBuffer::copy({span_data_expression(arg)}, {element_count_expression(arg)}));\n")
        else:
            cpp_element = {"GLfloat": "float", "GLint": "i32", "GLuint": "u32", "GLenum": "u32", "GLboolean": "u8"}[element]
            out.write(f"    Vector<{cpp_element}> {message_param_name}_value;\n")
            out.write(f"    if ({name})\n        {message_param_name}_value.append({span_data_expression(arg)}, {element_count_expression(arg)});\n")
        if arg.get("nullable"):
            return [f"{name} != nullptr", f"move({message_param_name}_value)"]
        return [f"move({message_param_name}_value)"]
    return emit_command_argument(out, function, arg, message_param_name)


def emit_command_method(out: TextIO, function: dict) -> None:
    out.write(f"{method_signature(function, 'WebGLContextProxy::')}\n{{\n")
    out.write("    if (is_lost())\n        return;\n")
    body = StringIO()
    call_arguments = ["webgl_context_id()"]
    for arg in function["args"]:
        message_param_name = ipc_parameter_name(snake_case(arg["name"]))
        call_arguments.extend(emit_command_argument(body, function, arg, message_param_name))

    payload_args = [arg for arg in function["args"] if "payload" in arg or (arg.get("object") and is_const_pointer(arg))]
    if payload_args:
        total = " + ".join(f"static_cast<size_t>({arg['payload']})" for arg in payload_args)
        guarded = " && ".join(f"{arg['name']}" for arg in payload_args if arg.get("nullable")) or None
        out.write(body.getvalue())
        condition = f"({total}) > max_single_message_payload_bytes"
        if guarded:
            condition = f"({guarded}) && {condition}"
        out.write(f"    if ({condition}) {{\n")
        out.write('        dbgln("FIXME: Dropping oversized WebGL call; chunked uploads are not implemented yet");\n')
        out.write("        return;\n    }\n")
    else:
        out.write(body.getvalue())
    out.write(f"    transport().{ipc_message_name(function)}({', '.join(call_arguments)});\n")
    out.write("}\n\n")


def emit_gen_method(out: TextIO, function: dict) -> None:
    out.write(f"{method_signature(function, 'WebGLContextProxy::')}\n{{\n")
    if function["return"] != "void":
        result = "reinterpret_cast<GLsync>(static_cast<uintptr_t>(id))" if function["return"] == "GLsync" else "id"
        scalars = "".join(f", {arg['name']}" for arg in function["args"])
        out.write(f"""    auto id = allocate_object_id();
    if (!is_lost())
        transport().{ipc_message_name(function)}(webgl_context_id(), id{scalars});
    return {result};
}}

""")
        return
    count_name = function["args"][0]["name"]
    out_name = function["args"][1]["name"]
    out.write(f"""    if ({count_name} <= 0)
        return;
    Vector<u32> allocated_ids;
    allocated_ids.ensure_capacity({count_name});
    for (GLsizei i = 0; i < {count_name}; ++i) {{
        auto id = allocate_object_id();
        allocated_ids.unchecked_append(id);
        {out_name}[i] = id;
    }}
    if (!is_lost())
        transport().{ipc_message_name(function)}(webgl_context_id(), {count_name}, allocated_ids);
}}

""")


def emit_sync_method(out: TextIO, function: dict) -> None:
    out.write(f"{method_signature(function, 'WebGLContextProxy::')}\n{{\n")
    has_return = function["return"] != "void"
    if has_return:
        out.write(f"    if (is_lost())\n        return {{}};\n")
    else:
        out.write("    if (is_lost())\n        return;\n")

    call_arguments = ["webgl_context_id()"]
    body = StringIO()
    for arg in function["args"]:
        if arg.get("out"):
            continue
        message_param_name = ipc_parameter_name(snake_case(arg["name"]))
        call_arguments.extend(emit_sync_argument(body, function, arg, message_param_name))
    out.write(body.getvalue())

    responses = ipc_response_params(function)
    returns_first = bool(responses) and (len(responses) == 1 or responses[0][1] == "return_value")
    reference_responses = responses[1:] if returns_first else responses

    out_args = [arg for arg in function["args"] if arg.get("out")]
    locals_by_name = {}
    for ipc_type, name, _ in reference_responses:
        out.write(f"    {ipc_type} {name}_reply {{}};\n")
        call_arguments.append(f"{name}_reply")
        locals_by_name[name] = f"{name}_reply"

    invocation = f"transport().{ipc_message_name(function)}({', '.join(call_arguments)})"
    if returns_first:
        out.write(f"    auto return_value = {invocation};\n")
    else:
        out.write(f"    {invocation};\n")

    # Copy reply data back into the caller's buffers; never more than asked for.
    for arg in out_args:
        name = arg["name"]
        reply_name = locals_by_name.get(ipc_parameter_name(snake_case(name)))
        if reply_name is None:
            continue  # delivered via the return value (single-output calls)
        if "payload" in arg:
            element = gl_element_type(arg["type"])
            if element == "GLchar":
                out.write(f"""    if ({name}) {{
        auto reply_bytes = {reply_name}.bytes();
        auto copy_count = min(reply_bytes.size(), {arg['payload']} > 0 ? static_cast<size_t>({arg['payload']}) - 1 : 0);
        __builtin_memcpy({name}, reply_bytes.data(), copy_count);
        if (static_cast<size_t>({arg['payload']}) > 0)
            {name}[copy_count] = 0;
    }}
""")
            else:
                copy_cast = "reinterpret_cast<GLint64*>" if element == "GLint64" else ""
                _ = copy_cast
                out.write(f"""    if ({name})
        __builtin_memcpy({name}, {reply_name}.data(), min({reply_name}.size() * sizeof({reply_name}[0]), static_cast<size_t>({arg['payload']})));
""")
        else:
            out.write(f"    if ({name})\n        *{name} = {reply_name};\n")

    if returns_first and len(responses) == 1 and responses[0][1] != "return_value":
        # Single-output call whose value came back as the return: write it to the out arg.
        ipc_type, name, _ = responses[0]
        matching = next(arg for arg in out_args if ipc_parameter_name(snake_case(arg["name"])) == name)
        arg_name = matching["name"]
        if "payload" in matching:
            out.write(f"""    if ({arg_name})
        __builtin_memcpy({arg_name}, return_value.data(), min(return_value.size() * sizeof(return_value[0]), static_cast<size_t>({matching['payload']})));
""")
        else:
            out.write(f"    if ({arg_name})\n        *{arg_name} = return_value;\n")
        out.write("}\n\n")
        return

    if has_return:
        out.write("    return return_value;\n")
    out.write("}\n\n")


def write_header_file(out: TextIO, functions: list) -> None:
    out.write("""#pragma once

// GL type and enum definitions only -- GL_GLEXT_PROTOTYPES stays undefined; nothing in
// WebContent calls GL directly.
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
extern "C" {
#include <GLES2/gl2ext_angle.h>
}
#include <GLES3/gl3.h>

#include <LibWeb/WebGL/WebGLContextProxyBase.h>

namespace Web::WebGL {

// The remote implementation of the GL seam: identical signatures to GLFunctions, so the
// WebGL implementation files cannot tell the difference.
class WEB_API WebGLContextProxy final : public WebGLContextProxyBase {
public:
    using WebGLContextProxyBase::WebGLContextProxyBase;

""")
    for function in functions:
        if function["category"] in ("command", "gen", "sync"):
            out.write(f"    {method_signature(function)};\n")
    out.write("""
    // Custom-handled entry points; defined manually in WebGLContextProxyBase.cpp.
""")
    for function in functions:
        if function["category"] == "custom":
            out.write(f"    {method_signature(function)};\n")
    out.write("""};

}
""")


def write_implementation_file(out: TextIO, functions: list) -> None:
    out.write("""#include <AK/Vector.h>
#include <LibWeb/WebGL/RemoteWebGLTransport.h>
#include <LibWeb/WebGL/WebGLContextProxy.h>

namespace Web::WebGL {

""")
    for function in functions:
        if function["category"] == "command":
            emit_command_method(out, function)
        elif function["category"] == "gen":
            emit_gen_method(out, function)
        elif function["category"] == "sync":
            emit_sync_method(out, function)
    out.write("}\n")


def main():
    parser = argparse.ArgumentParser(description="Generate the WebGL context proxy", add_help=False)
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

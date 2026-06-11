#!/usr/bin/env python3

# Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
#
# SPDX-License-Identifier: BSD-2-Clause

import argparse
import re
import sys

from io import StringIO
from pathlib import Path
from typing import TextIO

sys.path.append(str(Path(__file__).resolve().parent))

from libweb_webgl import gl_element_type
from libweb_webgl import ipc_message_name
from libweb_webgl import ipc_message_params
from libweb_webgl import ipc_response_params
from libweb_webgl import is_const_pointer
from libweb_webgl import is_ipc_command
from libweb_webgl import is_ipc_sync
from libweb_webgl import ipc_parameter_name
from libweb_webgl import is_pointer
from libweb_webgl import load_functions
from libweb_webgl import snake_case

# Generates WebGLMessageDispatcher: the layer between IPC::ConnectionFromClient and the
# hand-written ConnectionFromWebContent that implements one handler per WebGL message.
# Handlers look up the target context, make its GL context current if needed, translate
# client object ids, validate payload sizes against what the message's own scalar
# arguments imply, and call the GL seam. GL-level validation stays in ANGLE.
#
# Wire-specified ops (customs and builtins) are declared here but defined by hand in
# WebGLDispatcherCustomMessages.cpp; webgl_present_to_compositor is left pure because
# publishing needs CompositorState, which only the final connection class has.

PRESENT_METHOD = "present_to_compositor"


def pascal_case(message_name: str) -> str:
    return "".join(part.capitalize() for part in message_name.split("_"))


def is_generated_here(function: dict) -> bool:
    return function["category"] in ("command", "gen", "sync")


def is_declared_only(function: dict) -> bool:
    if function["method"] == PRESENT_METHOD:
        return False
    return (is_ipc_command(function) or is_ipc_sync(function)) and not is_generated_here(function)


def handler_signature(function: dict, qualifier: str = "") -> str:
    parameters = ", ".join(f"{ipc_type} {name}" for ipc_type, name, _ in [("u64", "webgl_context_id", None)] + ipc_message_params(function))
    # Synchronous messages with no outputs get a void handler (no Response type exists).
    if is_ipc_sync(function) and ipc_response_params(function):
        return_type = f"Messages::CompositorWebContentServer::{pascal_case(ipc_message_name(function))}Response"
    else:
        return_type = "void"
    return f"{return_type} {qualifier}{ipc_message_name(function)}({parameters})"


def zeroed_response(function: dict) -> str:
    zeros = []
    for ipc_type, _, _ in ipc_response_params(function):
        if ipc_type in ("String",):
            zeros.append("String {}")
        elif ipc_type.startswith("Vector<") or ipc_type == "ByteBuffer":
            zeros.append("{}")
        elif ipc_type == "bool":
            zeros.append("false")
        else:
            zeros.append("0")
    return "{ " + ", ".join(zeros) + " }"


def failure_return(function: dict) -> str:
    if is_ipc_sync(function) and ipc_response_params(function):
        return f"return {zeroed_response(function)};"
    return "return;"


def rewrite_size_expression(expression: str, function: dict) -> str:
    for arg_name in sorted((a["name"] for a in function["args"]), key=len, reverse=True):
        expression = re.sub(rf"\b{re.escape(arg_name)}\b", ipc_parameter_name(snake_case(arg_name)), expression)
    return expression


def emit_misbehave_check(out: TextIO, function: dict, condition: str, reason: str) -> None:
    out.write(f"    if ({condition}) {{\n")
    out.write(f'        did_misbehave("{reason}");\n')
    out.write(f"        {failure_return(function)}\n")
    out.write("    }\n")


# Emits id/payload translation for one in-argument; returns the GL call argument
# expression.
def emit_argument(out: TextIO, function: dict, arg: dict) -> str:
    name = ipc_parameter_name(snake_case(arg["name"]))
    if arg.get("object") and not is_pointer(arg):
        lookup = "lookup_sync" if arg["type"] == "GLsync" else "lookup"
        out.write(f"    auto {name}_or_error = context->objects().{lookup}({name});\n")
        emit_misbehave_check(out, function, f"{name}_or_error.is_error()", "WebContent referenced an unknown WebGL object id")
        if arg.get("zero_means_default"):
            default_getter = "default_framebuffer" if arg["name"] == "framebuffer" else "default_renderbuffer"
            out.write(f"    auto {name}_name = {name} ? {name}_or_error.value() : context->gl_context().{default_getter}();\n")
        else:
            out.write(f"    auto {name}_name = {name}_or_error.value();\n")
        return f"{name}_name"
    if arg.get("object") and is_const_pointer(arg):
        deletes = "take" if function.get("deletes_objects") else "lookup"
        out.write(f"    Vector<GLuint> {name}_names;\n")
        out.write(f"    {name}_names.ensure_capacity({name}.size());\n")
        out.write(f"    for (auto id : {name}) {{\n")
        out.write(f"        auto name_or_error = context->objects().{deletes}(id);\n")
        out.write("        if (name_or_error.is_error()) {\n")
        out.write('            did_misbehave("WebContent referenced an unknown WebGL object id");\n')
        out.write(f"            {failure_return(function)}\n")
        out.write("        }\n")
        out.write(f"        {name}_names.unchecked_append(name_or_error.value());\n")
        out.write("    }\n")
        return f"{name}_names.data()"
    if arg.get("string"):
        out.write(f"    auto {name}_string = {name}.to_byte_string();\n")
        return f"{name}_string.characters()"
    if arg.get("offset"):
        return f"reinterpret_cast<void const*>(static_cast<uintptr_t>({name}))"
    if "payload" in arg:
        expression = rewrite_size_expression(arg["payload"], function)
        element = gl_element_type(arg["type"])
        if arg.get("nullable"):
            emit_misbehave_check(out, function, f"has_{name} && static_cast<i64>({name}.size()) != static_cast<i64>({expression})", "WebGL payload size mismatch")
            emit_misbehave_check(out, function, f"!has_{name} && !{name}.is_empty()", "WebGL message has unexpected payload")
            return f"(has_{name} ? {name}.data() : nullptr)"
        if element == "void":
            emit_misbehave_check(out, function, f"static_cast<i64>({name}.size()) != static_cast<i64>({expression})", "WebGL payload size mismatch")
            return f"{name}.data()"
        emit_misbehave_check(out, function, f"static_cast<i64>({name}.size() * sizeof({element})) != static_cast<i64>({expression})", "WebGL payload size mismatch")
        return f"{name}.data()"
    return name


def emit_command_body(out: TextIO, function: dict) -> None:
    call_arguments = []
    for arg in function["args"]:
        call_arguments.append(emit_argument(out, function, arg))
    out.write(f"    context->gl_context().{function['method']}({', '.join(call_arguments)});\n")
    if function.get("deletes_objects"):
        pass  # object map entries were already removed by take() above


def emit_gen_body(out: TextIO, function: dict) -> None:
    if function["return"] != "void":
        scalars = ", ".join(ipc_parameter_name(snake_case(a["name"])) for a in function["args"])
        add = "add_sync" if function["return"] == "GLsync" else "add"
        out.write(f"    if (context->objects().{add}(id, context->gl_context().{function['method']}({scalars})).is_error())\n")
        out.write('        did_misbehave("WebContent reused a WebGL object id");\n')
        return
    count = ipc_parameter_name(snake_case(function["args"][0]["name"]))
    ids = ipc_parameter_name(snake_case(function["args"][1]["name"]))
    emit_misbehave_check(out, function, f"static_cast<size_t>({count}) != {ids}.size()", "WebGL object id count mismatch")
    out.write(f"    for (auto id : {ids}) {{\n")
    out.write("        GLuint name = 0;\n")
    out.write(f"        context->gl_context().{function['method']}(1, &name);\n")
    out.write("        if (context->objects().add(id, name).is_error()) {\n")
    out.write('            did_misbehave("WebContent reused a WebGL object id");\n')
    out.write("            return;\n")
    out.write("        }\n")
    out.write("    }\n")


def emit_sync_body(out: TextIO, function: dict) -> None:
    call_arguments = []
    response_expressions = {}

    for arg in function["args"]:
        name = ipc_parameter_name(snake_case(arg["name"]))
        if not arg.get("out"):
            call_arguments.append(emit_argument(out, function, arg))
            continue
        if "payload" in arg:
            element = gl_element_type(arg["type"])
            expression = rewrite_size_expression(arg["payload"], function)
            out.write(f"    auto {name}_byte_size = static_cast<i64>({expression});\n")
            emit_misbehave_check(out, function, f"{name}_byte_size < 0 || static_cast<u64>({name}_byte_size) > max_webgl_sync_reply_size", "WebGL reply would be too large")
            if element == "GLchar":
                out.write(f"    Vector<GLchar> {name}_buffer;\n")
                out.write(f"    {name}_buffer.resize(static_cast<size_t>({name}_byte_size));\n")
                call_arguments.append(f"{name}_buffer.data()")
                response_expressions[name] = "__string__"
            elif element == "GLint64":
                out.write(f"    Vector<i64> {name};\n")
                out.write(f"    {name}.resize(static_cast<size_t>({name}_byte_size) / sizeof(GLint64));\n")
                call_arguments.append(f"reinterpret_cast<GLint64*>({name}.data())")
                response_expressions[name] = f"move({name})"
            else:
                cpp_element = {"GLfloat": "float", "GLint": "i32", "GLuint": "u32", "GLenum": "u32", "GLboolean": "u8"}[element]
                out.write(f"    Vector<{cpp_element}> {name};\n")
                out.write(f"    {name}.resize(static_cast<size_t>({name}_byte_size) / sizeof({element}));\n")
                call_arguments.append(f"{name}.data()")
                response_expressions[name] = f"move({name})"
        else:
            gl_type = arg["type"].replace("*", "").strip()
            out.write(f"    {gl_type} {name} {{}};\n")
            call_arguments.append(f"&{name}")
            response_expressions[name] = name

    invocation = f"context->gl_context().{function['method']}({', '.join(call_arguments)})"
    if function["return"] != "void":
        out.write(f"    auto return_value = {invocation};\n")
        response_expressions["return_value"] = "return_value"
    else:
        out.write(f"    {invocation};\n")

    if not ipc_response_params(function):
        return
    values = []
    for _, name, arg in ipc_response_params(function):
        expression = response_expressions[name]
        if expression == "__string__":
            length = "length" if "length" in response_expressions else f"{name}_buffer.size()"
            values.append(f"String::from_utf8_with_replacement_character(StringView {{ {name}_buffer.data(), min(static_cast<size_t>({length}), {name}_buffer.size()) }})")
        else:
            values.append(expression)
    out.write(f"    return {{ {', '.join(values)} }};\n")


def write_header_file(out: TextIO, functions: list) -> None:
    out.write("""#pragma once

#include <Compositor/CompositorWebContentClientEndpoint.h>
#include <Compositor/CompositorWebContentServerEndpoint.h>
#include <Compositor/WebGLHost.h>
#include <LibIPC/ConnectionFromClient.h>

namespace Compositor {

// Implements one handler per WebGL message, between IPC::ConnectionFromClient and the
// hand-written ConnectionFromWebContent (which keeps everything that needs
// CompositorState, including webgl_present_to_compositor).
class WebGLMessageDispatcher : public IPC::ConnectionFromClient<CompositorWebContentClientEndpoint, CompositorWebContentServerEndpoint> {
protected:
    WebGLMessageDispatcher(NonnullOwnPtr<IPC::Transport>, int client_id);

    virtual WebGLHost& webgl_host() = 0;

private:
    HostWebGLContext* webgl_context_for_message(u64 webgl_context_id);

""")
    for function in functions:
        if is_generated_here(function):
            out.write(f"    virtual {handler_signature(function)} override;\n")
    out.write("""
    // Wire-specified ops; defined by hand in WebGLDispatcherCustomMessages.cpp.
""")
    for function in functions:
        if is_declared_only(function):
            out.write(f"    virtual {handler_signature(function)} override;\n")
    out.write("""};

}
""")


def write_implementation_file(out: TextIO, functions: list) -> None:
    out.write("""#include <AK/Vector.h>
#include <Compositor/WebGLMessageDispatcher.h>

namespace Compositor {

using namespace Web::WebGL;

WebGLMessageDispatcher::WebGLMessageDispatcher(NonnullOwnPtr<IPC::Transport> transport, int client_id)
    : IPC::ConnectionFromClient<CompositorWebContentClientEndpoint, CompositorWebContentServerEndpoint>(*this, move(transport), client_id)
{
}

HostWebGLContext* WebGLMessageDispatcher::webgl_context_for_message(u64 webgl_context_id)
{
    auto* context = webgl_host().context(webgl_context_id);
    if (!context) {
        did_misbehave("WebContent sent a message for an unknown WebGL context");
        return nullptr;
    }
    context->make_current_if_needed();
    return context;
}
""")
    for function in functions:
        if not is_generated_here(function):
            continue
        body = StringIO()
        if function["category"] == "gen":
            emit_gen_body(body, function)
        elif function["category"] == "sync":
            emit_sync_body(body, function)
        else:
            emit_command_body(body, function)
        out.write(f"\n{handler_signature(function, 'WebGLMessageDispatcher::')}\n{{\n")
        out.write("    auto* context = webgl_context_for_message(webgl_context_id);\n")
        out.write("    if (!context)\n")
        out.write(f"        {failure_return(function)}\n")
        out.write(body.getvalue())
        out.write("}\n")
    out.write("\n}\n")


def main():
    parser = argparse.ArgumentParser(description="Generate the per-call WebGL message dispatcher", add_help=False)
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

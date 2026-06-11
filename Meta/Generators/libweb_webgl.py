# Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
#
# SPDX-License-Identifier: BSD-2-Clause

# Shared model for the WebGL generators (generate_libweb_webgl_functions.py,
# generate_libweb_webgl_commands.py): loads GLFunctions.json and derives the
# command-struct shape each annotated GL function serializes to.

import json


def load_functions(path: str) -> list:
    with open(path, "r", encoding="utf-8") as input_file:
        return json.load(input_file)


def snake_case(name: str) -> str:
    out = []
    for i, c in enumerate(name):
        if c.isupper() and i > 0:
            prev = name[i - 1]
            nxt = name[i + 1] if i + 1 < len(name) else ""
            if prev.islower() or (prev.isupper() and nxt.islower()):
                out.append("_")
        out.append(c.lower())
    return "".join(out)


def command_name(function: dict) -> str:
    if function["category"].startswith("builtin"):
        return function["name"]
    assert function["name"].startswith("gl")
    return function["name"][2:]


# Entries carried by the command stream: regular commands and object creation (struct
# shapes derived from the GL signature) plus wire-specified ops (custom-handled GL
# functions and builtins, whose struct shapes are spelled out in the JSON).
def is_wire_command(function: dict) -> bool:
    return "wire_command" in function


def is_wire_sync(function: dict) -> bool:
    return "wire_request" in function


def command_stream_entries(functions: list) -> list:
    return [f for f in functions if f["category"] in ("command", "gen") or is_wire_command(f)]


def sync_call_entries(functions: list) -> list:
    return [f for f in functions if f["category"] == "sync" or is_wire_sync(f)]


def is_pointer(arg: dict) -> bool:
    return arg["type"].endswith("*")


def is_const_pointer(arg: dict) -> bool:
    return is_pointer(arg) and "const" in arg["type"]


def deref_type(pointer_type: str) -> str:
    return pointer_type.replace("*", "").strip()


# Returns the ordered (cpp_type, field_name, arg_or_none) triples of a synchronous
# call's request struct: every non-out argument, with object ids, strings, and input
# payloads in their wire representations.
def sync_request_fields(function: dict) -> list:
    fields = []
    for arg in function["args"]:
        if arg.get("out"):
            continue
        field_name = snake_case(arg["name"])
        if arg.get("object") and not is_pointer(arg):
            fields.append(("WebGLObjectId", field_name, arg))
        elif arg.get("string") or "payload" in arg:
            fields.append(("WebGLDataSpan", field_name, arg))
        else:
            assert not is_pointer(arg), f"unhandled pointer arg {function['name']}.{arg['name']}"
            fields.append((arg["type"], field_name, arg))
    return fields


# Returns the ordered (cpp_type, field_name, arg_or_none) triples of a synchronous
# call's reply struct: the return value, scalar outs by value, buffer outs as spans
# into the reply's inline data.
def sync_reply_fields(function: dict) -> list:
    fields = []
    if function["return"] != "void":
        fields.append((function["return"], "return_value", None))
    for arg in function["args"]:
        if not arg.get("out"):
            continue
        field_name = snake_case(arg["name"])
        if "payload" in arg:
            fields.append(("WebGLDataSpan", field_name, arg))
        else:
            fields.append((deref_type(arg["type"]), field_name, arg))
    return fields


# Returns the ordered (cpp_type, field_name, arg_or_none) triples of the
# trivially-copyable struct a command/gen function serializes to.
def command_struct_fields(function: dict) -> list:
    fields = []
    if function["category"] == "gen" and function["return"] != "void":
        fields.append(("WebGLObjectId", "id", None))
    for arg in function["args"]:
        field_name = snake_case(arg["name"])
        if function["category"] == "gen" and is_pointer(arg) and not is_const_pointer(arg):
            fields.append(("WebGLDataSpan", field_name, arg))  # client-allocated ids
        elif arg.get("object") and not is_pointer(arg):
            fields.append(("WebGLObjectId", field_name, arg))
        elif arg.get("object") and is_const_pointer(arg):
            fields.append(("WebGLDataSpan", field_name, arg))  # array of client ids
        elif arg.get("string"):
            fields.append(("WebGLDataSpan", field_name, arg))  # NUL-terminated bytes
        elif arg.get("offset"):
            fields.append(("GLintptr", field_name, arg))
        elif "payload" in arg:
            if arg.get("nullable"):
                fields.append(("bool", "has_" + field_name, arg))
            fields.append(("WebGLDataSpan", field_name, arg))
        else:
            assert not is_pointer(arg), f"unhandled pointer arg {function['name']}.{arg['name']}"
            fields.append((arg["type"], field_name, arg))
    return fields


# --- Per-call LibIPC transport mapping -------------------------------------------------
# Every WebGL call travels as an individual IPC message; these helpers derive the
# message parameter and response lists from the same annotations.

GL_SCALAR_TO_IPC = {
    "GLenum": "u32",
    "GLbitfield": "u32",
    "GLuint": "u32",
    "GLint": "i32",
    "GLsizei": "i32",
    "GLintptr": "i64",
    "GLsizeiptr": "i64",
    "GLint64": "i64",
    "GLuint64": "u64",
    "GLfloat": "float",
    "GLboolean": "bool",
    # Wire-spec types are already IPC-native or trivially mapped:
    "WebGLObjectId": "u32",
    "String": "String",
    "Vector<String>": "Vector<String>",
    "ByteBuffer": "ByteBuffer",
    "Vector<GLuint>": "Vector<u32>",
    "u64": "u64",
    "bool": "bool",
    "int": "i32",
}

# Element type of a `T const*` (or out `T*`) data argument -> IPC container.
GL_ELEMENT_TO_IPC_CONTAINER = {
    "GLfloat": "Vector<float>",
    "GLint": "Vector<i32>",
    "GLuint": "Vector<u32>",
    "GLenum": "Vector<u32>",
    "GLboolean": "Vector<u8>",
    "GLint64": "Vector<i64>",
    "void": "ByteBuffer",
    "GLchar": "String",
}


def gl_element_type(pointer_type: str) -> str:
    return pointer_type.replace("const", "").replace("*", "").strip()


def ipc_container_for(arg: dict) -> str:
    return GL_ELEMENT_TO_IPC_CONTAINER[gl_element_type(arg["type"])]


def ipc_message_name(function: dict) -> str:
    return "webgl_" + function["method"]


# The IPC compiler uses these as locals in generated encode/decode/dispatch bodies, so
# message parameters must not shadow them (a parameter literally named "buffer" breaks
# the generated static_encode).
IPC_RESERVED_PARAMETER_NAMES = {"attachments", "buffer", "stream", "decoder", "response", "result", "message"}


def ipc_parameter_name(name: str) -> str:
    if name in IPC_RESERVED_PARAMETER_NAMES:
        return name + "_value"
    return name


# Request parameters, excluding the leading `u64 webgl_context_id`, as
# (ipc_type, name, arg_or_none) triples.
def ipc_message_params(function: dict) -> list:
    params = []
    wire_fields = function.get("wire_command") or function.get("wire_request")
    if wire_fields is not None:
        for field in wire_fields:
            params.append((GL_SCALAR_TO_IPC[field["type"]], ipc_parameter_name(field["name"]), field))
        return params

    if function["category"] == "gen":
        if function["return"] != "void":
            params.append(("u32", "id", None))
            for arg in function["args"]:
                params.append((GL_SCALAR_TO_IPC[arg["type"]], ipc_parameter_name(snake_case(arg["name"])), arg))
            return params
        # glGen*(GLsizei n, GLuint* out): the client-allocated ids travel in the message.
        count = function["args"][0]
        out = function["args"][1]
        params.append((GL_SCALAR_TO_IPC[count["type"]], ipc_parameter_name(snake_case(count["name"])), count))
        params.append(("Vector<u32>", ipc_parameter_name(snake_case(out["name"])), out))
        return params

    for arg in function["args"]:
        if arg.get("out"):
            continue
        name = ipc_parameter_name(snake_case(arg["name"]))
        if arg.get("object") and not is_pointer(arg):
            params.append(("u32", name, arg))
        elif arg.get("object") and is_const_pointer(arg):
            params.append(("Vector<u32>", name, arg))
        elif arg.get("string"):
            params.append(("String", name, arg))
        elif arg.get("offset"):
            params.append(("i64", name, arg))
        elif "payload" in arg:
            if arg.get("nullable"):
                params.append(("bool", "has_" + name, None))
            params.append((ipc_container_for(arg), name, arg))
        else:
            params.append((GL_SCALAR_TO_IPC[arg["type"]], name, arg))
    return params


# Response values of a synchronous message as (ipc_type, name, arg_or_none) triples.
def ipc_response_params(function: dict) -> list:
    params = []
    if "wire_reply" in function:
        for field in function["wire_reply"]:
            params.append((GL_SCALAR_TO_IPC[field["type"]], ipc_parameter_name(field["name"]), field))
        return params
    if function["return"] != "void":
        params.append((GL_SCALAR_TO_IPC[function["return"]], "return_value", None))
    for arg in function["args"]:
        if not arg.get("out"):
            continue
        name = ipc_parameter_name(snake_case(arg["name"]))
        if "payload" in arg:
            params.append((ipc_container_for(arg), name, arg))
        else:
            params.append((GL_SCALAR_TO_IPC[deref_type(arg["type"])], name, arg))
    return params


def is_ipc_command(function: dict) -> bool:
    return function["category"] in ("command", "gen") or "wire_command" in function


def is_ipc_sync(function: dict) -> bool:
    return function["category"] == "sync" or "wire_request" in function


# The C++ type the generated senders (and therefore the transport interface) take for an
# IPC parameter type: containers go zero-copy.
def cpp_sender_type(ipc_type: str) -> str:
    if ipc_type.startswith("Vector<"):
        return f"ReadonlySpan<{ipc_type[len('Vector<'):-1]}>"
    if ipc_type == "String":
        return "StringView"
    if ipc_type == "ByteBuffer":
        return "ReadonlyBytes"
    return ipc_type

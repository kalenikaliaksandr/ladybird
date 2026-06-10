# Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
#
# SPDX-License-Identifier: BSD-2-Clause

# Shared model for the WebGL generators (generate_libweb_webgl_functions.py,
# generate_libweb_webgl_commands.py): loads GLFunctions.json and derives the
# command-struct shape each annotated GL function serializes to.

import json
import sys

from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parent.parent))

from Utils.utils import title_case_to_snake_case as snake_case


def load_functions(path: str) -> list:
    with open(path, "r", encoding="utf-8") as input_file:
        return json.load(input_file)


def command_name(function: dict) -> str:
    assert function["name"].startswith("gl")
    return function["name"][2:]


def method_name(function: dict) -> str:
    return snake_case(command_name(function))


# The GLFunctions and WebGLContextProxy classes must stay signature-identical (the WebGL
# implementation cannot tell them apart), so both generators emit from this one helper.
def method_signature(function: dict, qualifier: str = "") -> str:
    args = ", ".join(f"{arg['type']} {arg['name']}" for arg in function["args"])
    return f"{function['return']} {qualifier}{method_name(function)}({args})"


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

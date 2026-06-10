#!/usr/bin/env python3

# Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
#
# SPDX-License-Identifier: BSD-2-Clause

import argparse
import sys

from pathlib import Path
from typing import TextIO

sys.path.append(str(Path(__file__).resolve().parent))

from libweb_webgl import load_functions
from libweb_webgl import method_signature

# Generates Web::WebGL::GLFunctions from GLFunctions.json: one member function per GL
# entry point used by the WebGL implementation. This is the only place in LibWeb that is
# allowed to call GL entry points directly; everything above it goes through the methods
# so the GL boundary stays in one generated, mechanically-verifiable layer.


def write_header_file(out: TextIO, functions: list) -> None:
    out.write("""#pragma once

// GL type and enum definitions only -- GL_GLEXT_PROTOTYPES stays undefined here so that
// nothing outside the generated GLFunctions implementation can call GL directly.
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
extern "C" {
#include <GLES2/gl2ext_angle.h>
}
#include <GLES3/gl3.h>

#include <LibWeb/Export.h>
#include <LibWeb/WebGL/Types.h>

namespace Web::WebGL {

class WEB_API GLFunctions {
public:
""")

    for function in functions:
        out.write(f"    {method_signature(function)};\n")

    out.write("""};

}
""")


def write_implementation_file(out: TextIO, functions: list) -> None:
    out.write("""#define GL_GLEXT_PROTOTYPES 1
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
extern "C" {
#include <GLES2/gl2ext_angle.h>
}
#include <GLES3/gl3.h>

#include <LibWeb/WebGL/GLFunctions.h>

namespace Web::WebGL {
""")

    for function in functions:
        forwarded_args = ", ".join(arg["name"] for arg in function["args"])
        call = f"::{function['name']}({forwarded_args})"
        if function["return"] != "void":
            call = "return " + call
        out.write(f"""
{method_signature(function, "GLFunctions::")}
{{
    {call};
}}
""")

    out.write("""
}
""")


def main():
    parser = argparse.ArgumentParser(description="Generate WebGL GL function wrappers", add_help=False)
    parser.add_argument("--help", action="help", help="Show this help message and exit")
    parser.add_argument("-h", "--header", required=True, help="Path to the GLFunctions header file to generate")
    parser.add_argument(
        "-c", "--implementation", required=True, help="Path to the GLFunctions implementation file to generate"
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

# Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
#
# SPDX-License-Identifier: BSD-2-Clause

# Shared model for the WebGL generators: loads GLFunctions.json and derives the
# generated method name and signature of each GL entry point.

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


def method_signature(function: dict, qualifier: str = "") -> str:
    args = ", ".join(f"{arg['type']} {arg['name']}" for arg in function["args"])
    return f"{function['return']} {qualifier}{method_name(function)}({args})"

#!/usr/bin/env python3

# Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
#
# SPDX-License-Identifier: BSD-2-Clause

import argparse
import re
import sys

from pathlib import Path

sys.path.append(str(Path(__file__).resolve().parent))

from libweb_webgl import ipc_message_name
from libweb_webgl import ipc_message_params
from libweb_webgl import ipc_response_params
from libweb_webgl import is_ipc_command
from libweb_webgl import is_ipc_sync
from libweb_webgl import load_functions

# Splices one IPC message per WebGL call into the CompositorWebContentServer endpoint.
# An IPC connection carries exactly one endpoint pair, and remote WebGL relies on FIFO
# ordering with display-list updates, so the WebGL messages must live in the same
# endpoint as the rest of the WebContent<->Compositor traffic; the IPC compiler has no
# endpoint-merge mechanism, so this generator emits the complete merged .ipc file. The
# checked-in CompositorWebContentServer.ipc remains the source of truth for everything
# that is not a WebGL call; the WebGL block is appended after the template messages so
# existing message ids keep their values.


def main():
    parser = argparse.ArgumentParser(description="Generate the merged CompositorWebContentServer.ipc")
    parser.add_argument("--ipc", required=True, help="Path to the checked-in CompositorWebContentServer.ipc template")
    parser.add_argument("-j", "--json", required=True, help="Path to GLFunctions.json")
    parser.add_argument("-o", "--output", required=True, help="Path to the merged .ipc file to generate")
    args = parser.parse_args()

    with open(args.ipc, "r", encoding="utf-8") as input_file:
        template = input_file.read()

    functions = load_functions(args.json)

    closing_brace = template.rstrip().rfind("}")
    if closing_brace == -1 or "endpoint CompositorWebContentServer" not in template:
        raise RuntimeError("CompositorWebContentServer.ipc does not look like the expected endpoint template")

    template_message_names = set(re.findall(r"^\s*(\w+)\(", template, re.MULTILINE))

    lines = [
        "\n",
        "    // Everything below is spliced at build time by generate_compositor_webgl_ipc.py\n",
        "    // from Libraries/LibWeb/WebGL/GLFunctions.json: one message per WebGL call.\n",
    ]
    for function in functions:
        command = is_ipc_command(function)
        sync = is_ipc_sync(function)
        if not command and not sync:
            continue  # custom entries without a wire shape never travel (e.g. map/unmap)
        name = ipc_message_name(function)
        if name in template_message_names:
            raise RuntimeError(f"WebGL message '{name}' collides with a hand-written message")
        parameters = ", ".join(f"{ipc_type} {param_name}" for ipc_type, param_name, _ in [("u64", "webgl_context_id", None)] + ipc_message_params(function))
        if command:
            lines.append(f"    {name}({parameters}) =|\n")
        else:
            returns = ", ".join(f"{ipc_type} {param_name}" for ipc_type, param_name, _ in ipc_response_params(function))
            lines.append(f"    {name}({parameters}) => ({returns})\n")

    merged = template[:closing_brace] + "".join(lines) + "}\n"
    header = "// GENERATED FILE -- do not edit; the source of truth is Services/Compositor/CompositorWebContentServer.ipc plus Libraries/LibWeb/WebGL/GLFunctions.json.\n"

    with open(args.output, "w", encoding="utf-8") as output_file:
        output_file.write(header + merged)


if __name__ == "__main__":
    main()

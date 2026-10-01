#!/usr/bin/env bash
#
# Copyright (c) 2026-present, the Ladybird developers.
#
# SPDX-License-Identifier: BSD-2-Clause
#
# Regenerates the expectation tables of the numeric conversion, value operator and intrinsics tests from the C++
# runtime.
# Usage: regenerate.sh [path to the C++ js binary]
set -euo pipefail

oracle_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
js_binary="${1:-${oracle_directory}/../../../../../Build/release/bin/js}"

for generator in generate.js value_operators.js intrinsics.js; do
    "${js_binary}" --raw-strings --disable-ansi-colors "${oracle_directory}/${generator}" | awk -v directory="${oracle_directory}" '
        /^\/\/\/\/ FILE: / { output = directory "/" $3; printf "" > output; next }
        { print > output }
    '
done

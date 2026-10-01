#!/usr/bin/env bash
#
# Copyright (c) 2026-present, the Ladybird developers.
#
# SPDX-License-Identifier: BSD-2-Clause
#
# Regenerates the expectation tables of the numeric conversion tests from the C++ runtime.
# Usage: regenerate.sh [path to the C++ js binary]
set -euo pipefail

oracle_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
js_binary="${1:-${oracle_directory}/../../../../../Build/release/bin/js}"

"${js_binary}" --raw-strings --disable-ansi-colors "${oracle_directory}/generate.js" | awk -v directory="${oracle_directory}" '
    /^\/\/\/\/ FILE: / { output = directory "/" $3; printf "" > output; next }
    { print > output }
'

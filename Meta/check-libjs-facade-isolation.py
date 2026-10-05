#!/usr/bin/env python3
#
# Copyright (c) 2026-present, the Ladybird developers.
#
# SPDX-License-Identifier: BSD-2-Clause

"""Check that a LIBJS_RUNTIME=Rust build compiled nothing against the C++ LibJS runtime.

In such a build, LibJS is the facade over the Rust runtime, and the poison root stops code that names a C++ runtime
header as <LibJS/...> from compiling. This check goes by what the compiler actually read: the dependencies that ninja
recorded for every C and C++ translation unit it compiled. That also covers headers reached through a relative or
build-directory path, which the poison root never sees. For every translation unit:

- No file of Libraries/LibJS outside Libraries/LibJS/Facade, other than the files that both runtimes use verbatim
  (Libraries/LibJS/Facade/shared-files.txt).
- No LibJS header of the build directory other than Export.h and the runtime's generated Embedding/ headers.
- Embedding/ABI.h, the runtime's C ABI, only for the facade's own .cpp files: inline facade code reads
  Embedding/Layout.h, and everything else goes through the facade's out-of-line JS_API functions. The runtime's C ABI
  tests, which link the runtime instead of the facade, are exempt.
"""

import argparse
import os
import subprocess
import sys

from collections import defaultdict
from pathlib import Path

LIBJS_DIRECTORY = "Libraries/LibJS"
FACADE_DIRECTORY = "Libraries/LibJS/Facade"
SHARED_FILES_LIST = "Libraries/LibJS/Facade/shared-files.txt"
RUNTIME_C_ABI_TEST_DIRECTORY = "Tests/LibJS/Embedding"

GENERATED_EXPORT_HEADER = "Export.h"
GENERATED_EMBEDDING_DIRECTORY = "Embedding/"
RUNTIME_C_ABI_HEADER = "Embedding/ABI.h"

OBJECT_FILE_SUFFIXES = (".o", ".obj")
LISTED_TRANSLATION_UNITS_PER_DEPENDENCY = 10


def read_cmake_cache(build_directory):
    cache_path = build_directory / "CMakeCache.txt"
    if not cache_path.is_file():
        sys.exit(f"{build_directory} is not a CMake build directory: it has no CMakeCache.txt")
    entries = {}
    for line in cache_path.read_text(encoding="utf-8").splitlines():
        if line.startswith(("#", "//")):
            continue
        name_and_type, separator, value = line.partition("=")
        if separator:
            entries[name_and_type.partition(":")[0]] = value
    return entries


def read_shared_files(source_directory):
    lines = (source_directory / SHARED_FILES_LIST).read_text(encoding="utf-8").splitlines()
    return {line.strip() for line in lines if line.strip() and not line.startswith("#")}


def recorded_dependencies_of_compiled_sources(ninja, build_directory):
    """Yields, for each compiled C or C++ translation unit, the paths ninja recorded, its source file first."""
    process = subprocess.Popen([ninja, "-C", str(build_directory), "-t", "deps"], stdout=subprocess.PIPE, text=True)
    assert process.stdout is not None
    dependencies = None
    for line in process.stdout:
        if line.startswith(" "):
            if dependencies is not None:
                dependencies.append(line.strip())
            continue
        if dependencies:
            yield dependencies
        output, separator, _ = line.partition(": #deps ")
        dependencies = [] if separator and output.endswith(OBJECT_FILE_SUFFIXES) else None
    if dependencies:
        yield dependencies
    if process.wait() != 0:
        sys.exit(f"'{ninja} -t deps' failed in {build_directory}")


class IsolationViolations:
    def __init__(self):
        self.dependents_of_cpp_runtime_file = defaultdict(set)
        self.dependents_of_unexpected_generated_header = defaultdict(set)
        self.dependents_of_runtime_c_abi_header = set()

    def __bool__(self):
        return bool(
            self.dependents_of_cpp_runtime_file
            or self.dependents_of_unexpected_generated_header
            or self.dependents_of_runtime_c_abi_header
        )


def listed_dependents(dependents):
    sorted_dependents = sorted(dependents)
    lines = [f"  {dependent}" for dependent in sorted_dependents[:LISTED_TRANSLATION_UNITS_PER_DEPENDENCY]]
    if len(sorted_dependents) > LISTED_TRANSLATION_UNITS_PER_DEPENDENCY:
        lines.append(f"  ... and {len(sorted_dependents) - LISTED_TRANSLATION_UNITS_PER_DEPENDENCY} more")
    return lines


def print_violations(violations, build_directory):
    sections = []

    if violations.dependents_of_cpp_runtime_file:
        lines = []
        for path, dependents in sorted(violations.dependents_of_cpp_runtime_file.items()):
            lines.append(
                f"{LIBJS_DIRECTORY}/{path} belongs to the C++ LibJS runtime, but these translation units depend on it:"
            )
            lines += listed_dependents(dependents)
        lines.append(
            f"Include LibJS headers as <LibJS/...> so that they resolve to the facade, and add what is missing to "
            f"{FACADE_DIRECTORY}/LibJS. A file that both runtimes can use verbatim belongs in {SHARED_FILES_LIST}."
        )
        sections.append(lines)

    if violations.dependents_of_unexpected_generated_header:
        lines = []
        for path, dependents in sorted(violations.dependents_of_unexpected_generated_header.items()):
            lines.append(
                f"{build_directory}/{LIBJS_DIRECTORY}/{path} is not a header that a LIBJS_RUNTIME=Rust build "
                f"generates, but these translation units depend on it:"
            )
            lines += listed_dependents(dependents)
        lines.append(
            "A build directory that once built the C++ runtime keeps the headers that its build generated. Build each "
            "runtime in a build directory of its own."
        )
        sections.append(lines)

    if violations.dependents_of_runtime_c_abi_header:
        lines = [
            f"{build_directory}/{LIBJS_DIRECTORY}/{RUNTIME_C_ABI_HEADER} is the Rust runtime's C ABI, which only the "
            f"facade's .cpp files may use, but these translation units depend on it:"
        ]
        lines += listed_dependents(violations.dependents_of_runtime_c_abi_header)
        lines.append(
            "When a facade header includes it, every user of that header does. Facade headers may read "
            "Embedding/Layout.h, and call the runtime only through out-of-line JS_API functions of the facade."
        )
        sections.append(lines)

    print("\n\n".join("\n".join(lines) for lines in sections))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("build_directory", help="a LIBJS_RUNTIME=Rust build directory generated for Ninja")
    args = parser.parse_args()

    build_directory = Path(args.build_directory).resolve()
    cache = read_cmake_cache(build_directory)
    if cache.get("LIBJS_RUNTIME") != "Rust":
        sys.exit(f"{build_directory} is not a LIBJS_RUNTIME=Rust build, so it has no LibJS facade to check")
    if not cache.get("CMAKE_GENERATOR", "").startswith("Ninja"):
        sys.exit(f"{build_directory} is not a Ninja build, so it has no ninja deps log to check")
    source_directory = Path(cache["CMAKE_HOME_DIRECTORY"]).resolve()
    shared_files = read_shared_files(source_directory)

    libjs_source_prefix = f"{source_directory / LIBJS_DIRECTORY}{os.sep}"
    facade_source_prefix = f"{source_directory / FACADE_DIRECTORY}{os.sep}"
    runtime_c_abi_test_prefix = f"{source_directory / RUNTIME_C_ABI_TEST_DIRECTORY}{os.sep}"
    libjs_build_prefix = f"{build_directory / LIBJS_DIRECTORY}{os.sep}"

    resolved_paths = {}

    def resolve(recorded_path):
        resolved_path = resolved_paths.get(recorded_path)
        if resolved_path is None:
            resolved_path = os.path.realpath(os.path.join(build_directory, recorded_path))
            resolved_paths[recorded_path] = resolved_path
        return resolved_path

    def displayed(path):
        if path.startswith(f"{source_directory}{os.sep}"):
            return os.path.relpath(path, source_directory)
        return path

    violations = IsolationViolations()
    translation_unit_count = 0
    facade_translation_unit_count = 0

    ninja = cache.get("CMAKE_MAKE_PROGRAM") or "ninja"
    for dependencies in recorded_dependencies_of_compiled_sources(ninja, build_directory):
        source_file = resolve(dependencies[0])
        is_facade_source_file = source_file.startswith(facade_source_prefix)
        may_use_runtime_c_abi = is_facade_source_file or source_file.startswith(runtime_c_abi_test_prefix)
        translation_unit_count += 1
        if is_facade_source_file:
            facade_translation_unit_count += 1

        # Resolving every recorded path would be slow, and only paths that name LibJS can be violations.
        for path in (resolve(recorded_path) for recorded_path in dependencies if "LibJS" in recorded_path):
            if path.startswith(libjs_source_prefix) and not path.startswith(facade_source_prefix):
                path_in_libjs = path[len(libjs_source_prefix) :]
                if path_in_libjs not in shared_files:
                    violations.dependents_of_cpp_runtime_file[path_in_libjs].add(displayed(source_file))
            # The runtime's build also generates a source file there, which the users of the runtime compile.
            elif path.startswith(libjs_build_prefix) and path != source_file:
                path_in_libjs = path[len(libjs_build_prefix) :]
                if path_in_libjs == RUNTIME_C_ABI_HEADER:
                    if not may_use_runtime_c_abi:
                        violations.dependents_of_runtime_c_abi_header.add(displayed(source_file))
                elif path_in_libjs != GENERATED_EXPORT_HEADER and not path_in_libjs.startswith(
                    GENERATED_EMBEDDING_DIRECTORY
                ):
                    violations.dependents_of_unexpected_generated_header[path_in_libjs].add(displayed(source_file))

    if violations:
        print_violations(violations, build_directory)
    if facade_translation_unit_count == 0:
        if violations:
            print()
        print(f"{build_directory} has not compiled the LibJS facade yet. Build LibJS before checking it.")
    if violations or facade_translation_unit_count == 0:
        return 1

    print(
        f"None of the {translation_unit_count} translation units compiled in {build_directory}, "
        f"{facade_translation_unit_count} of them the LibJS facade's, depends on the C++ LibJS runtime."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

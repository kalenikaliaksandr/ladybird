#!/usr/bin/env python3
#
# Copyright (c) 2026-present, the Ladybird developers.
#
# SPDX-License-Identifier: BSD-2-Clause

"""Measure test262 parity between the C++ and Rust LibJS runtimes.

The run command drives the libjs-test262 runner the same way CI does and records which build, test262 checkout and
runner produced the results. The diff command compares two such runs file by file. A file that passes with the C++
runtime and not with the Rust one is a regression. Regressions in a locked directory fail the command; a directory is
locked once every file in it that passes with C++ also passes with Rust.
"""

import argparse
import json
import os
import re
import subprocess
import sys

from collections import Counter
from collections import defaultdict
from pathlib import Path

SOURCE_DIRECTORY = Path(__file__).resolve().parent.parent
DEFAULT_LOCKED_DIRECTORIES = SOURCE_DIRECTORY / "Libraries/LibJS/Runtime/Rust/test262-locked-directories.txt"
PASSED = "PASSED"


def git_revision(directory):
    process = subprocess.run(
        ["git", "-C", str(directory), "rev-parse", "HEAD"], capture_output=True, text=True, check=False
    )
    return process.stdout.strip() if process.returncode == 0 else None


def run(args):
    driver = Path(args.driver).resolve()
    test262 = Path(args.test262).resolve()
    output = Path(args.output).resolve()
    command = [
        args.python,
        str(driver / "main.py"),
        "--libjs-test262-runner",
        str(Path(args.runner).resolve()),
        "--test262-root",
        str(test262),
        "--silent",
        "--summary",
        "--timeout",
        str(args.timeout),
        "--per-file",
        str(output),
    ]
    if args.jobs:
        command += ["--concurrency", str(args.jobs)]
    if args.parse_only:
        command.append("--parse-only")
    for pattern in args.pattern:
        command += ["--pattern", pattern]
    environment = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
    process = subprocess.run(command, cwd=driver, env=environment, check=False)
    if process.returncode != 0 or not output.exists():
        print(f"The test262 driver failed with exit code {process.returncode}", file=sys.stderr)
        return 1
    results = json.loads(output.read_text())
    results["parity"] = {
        "ladybird_revision": git_revision(SOURCE_DIRECTORY),
        "test262_revision": git_revision(test262),
        "runner": str(Path(args.runner).resolve()),
        "parse_only": args.parse_only,
        "patterns": args.pattern,
    }
    output.write_text(json.dumps(results, indent=1, sort_keys=True))
    counts = Counter(results["results"].values())
    print(", ".join(f"{count} {result.lower()}" for result, count in counts.most_common()))
    return 0


def load_results(path):
    return json.loads(Path(path).read_text())["results"]


def load_list(path):
    if not path or not Path(path).exists():
        return []
    return [line.strip() for line in Path(path).read_text().splitlines() if line.strip() and not line.startswith("#")]


def is_in_directory(file, directory):
    return file == directory or file.startswith(directory.rstrip("/") + "/")


def parent_directories(file):
    parts = file.split("/")[:-1]
    return ["/".join(parts[:length]) for length in range(1, len(parts) + 1)]


def read_metadata(test262, file):
    try:
        text = (Path(test262) / file).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return []
    match = re.search(r"/\*---(.*?)---\*/", text, re.DOTALL)
    if not match:
        return []
    labels = []
    for key in ("features", "includes"):
        inline = re.search(rf"^{key}:\s*\[(.*?)\]", match.group(1), re.MULTILINE)
        if inline:
            labels += [f"{key}:{item.strip()}" for item in inline.group(1).split(",") if item.strip()]
            continue
        block = re.search(rf"^{key}:\s*\n((?:\s+-\s*.*\n?)+)", match.group(1), re.MULTILINE)
        if block:
            labels += [f"{key}:{item.strip()[1:].strip()}" for item in block.group(1).splitlines() if item.strip()]
    return labels


def suggest_locks(cpp, rust, regressions, locked):
    """The largest directories that have no regressions and are not locked yet."""
    directories_with_regressions = set()
    for file in regressions:
        directories_with_regressions.update(parent_directories(file))
    directories_with_passes = set()
    for file, result in cpp.items():
        if result == PASSED and file in rust:
            directories_with_passes.update(parent_directories(file))
    candidates = sorted(
        directories_with_passes - directories_with_regressions, key=lambda directory: directory.count("/")
    )
    suggestions = []
    for directory in candidates:
        if any(is_in_directory(directory, chosen) for chosen in suggestions + locked):
            continue
        suggestions.append(directory)
    return suggestions


def diff(args):
    cpp = load_results(args.cpp)
    rust = load_results(args.rust)
    locked = load_list(args.locked)
    flaky = set(load_list(args.flaky))
    files = sorted(set(cpp) & set(rust))
    regressions = [file for file in files if cpp[file] == PASSED and rust[file] != PASSED and file not in flaky]
    both_fail = [file for file in files if cpp[file] != PASSED and rust[file] != PASSED]
    rust_only_pass = [file for file in files if cpp[file] != PASSED and rust[file] == PASSED]
    locked_regressions = [file for file in regressions if any(is_in_directory(file, d) for d in locked)]

    print(f"{len(files)} files in both runs")
    print(
        f"Regressions (C++ passes, Rust does not): {len(regressions)}, of which in locked directories: {len(locked_regressions)}"
    )
    print(f"Both fail: {len(both_fail)}")
    print(f"Rust-only passes: {len(rust_only_pass)}")
    if missing := sorted(set(cpp) ^ set(rust)):
        print(f"Files in only one run: {len(missing)}")

    if args.directories:
        depth = args.directories
        table = defaultdict(lambda: Counter())
        for file in files:
            directory = "/".join(file.split("/")[: depth + 1])
            table[directory]["files"] += 1
            table[directory]["cpp"] += cpp[file] == PASSED
            table[directory]["rust"] += rust[file] == PASSED
            table[directory]["regressions"] += file in regressions
        print(f"\n{'directory':<60} {'files':>7} {'C++':>7} {'Rust':>7} {'regressed':>9}")
        for directory, counts in sorted(table.items()):
            print(
                f"{directory:<60} {counts['files']:>7} {counts['cpp']:>7} {counts['rust']:>7} {counts['regressions']:>9}"
            )

    if args.by_feature:
        labels = Counter()
        for file in regressions:
            labels.update(read_metadata(args.test262, file) or ["(none)"])
        print("\nRegressions by feature and harness include:")
        for label, count in labels.most_common(args.by_feature):
            print(f"{count:>7} {label}")

    if args.list:
        for title, group in (("Regressions", regressions), ("Rust-only passes", rust_only_pass)):
            print(f"\n{title}:")
            for file in group:
                print(f"  {file}: C++ {cpp[file]}, Rust {rust[file]}")

    if args.suggest_locks:
        print("\nDirectories that could be locked:")
        for directory in suggest_locks(cpp, rust, regressions, locked):
            print(f"  {directory}")

    if locked_regressions:
        print("\nRegressions in locked directories:", file=sys.stderr)
        for file in locked_regressions:
            print(f"  {file}: C++ {cpp[file]}, Rust {rust[file]}", file=sys.stderr)
        return 1
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)

    run_parser = commands.add_parser("run", help="Run test262 with one runner and record per-file results")
    run_parser.add_argument("--runner", required=True, help="test262-runner or test262-runner-rust")
    run_parser.add_argument("--driver", required=True, help="A libjs-test262 checkout")
    run_parser.add_argument("--test262", required=True, help="A test262 checkout")
    run_parser.add_argument("--output", required=True, help="Where to write the per-file results")
    run_parser.add_argument("--python", default=sys.executable, help="The Python that runs the driver")
    run_parser.add_argument("--pattern", action="append", default=[], help="Only run tests matching this glob")
    run_parser.add_argument("--parse-only", action="store_true")
    run_parser.add_argument("--jobs", type=int)
    run_parser.add_argument("--timeout", type=int, default=10)
    run_parser.set_defaults(handler=run)

    diff_parser = commands.add_parser("diff", help="Compare the per-file results of the C++ and Rust runtimes")
    diff_parser.add_argument("--cpp", required=True)
    diff_parser.add_argument("--rust", required=True)
    diff_parser.add_argument("--locked", default=DEFAULT_LOCKED_DIRECTORIES)
    diff_parser.add_argument("--flaky", help="Files whose results vary between runs, which are never regressions")
    diff_parser.add_argument("--test262", help="The test262 checkout, needed for --by-feature")
    diff_parser.add_argument("--directories", type=int, metavar="DEPTH", help="Print a table per directory")
    diff_parser.add_argument("--by-feature", type=int, metavar="COUNT", help="Group regressions by test metadata")
    diff_parser.add_argument("--list", action="store_true", help="List every regression and Rust-only pass")
    diff_parser.add_argument("--suggest-locks", action="store_true")
    diff_parser.set_defaults(handler=diff)

    args = parser.parse_args()
    if args.command == "diff" and args.by_feature and not args.test262:
        parser.error("--by-feature needs --test262")
    return args.handler(args)


if __name__ == "__main__":
    sys.exit(main())

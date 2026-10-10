#!/usr/bin/env python3
"""Run paired, independently verified Rust benchmark executables, not SQL APIs.

Build identical harnesses against separate before/after engine sources first.
Timings are observations; there are no speed thresholds or discarded repeats.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


def executable(directory, name):
    candidates = [p for p in directory.glob(name + "-*") if p.is_file() and os.access(p, os.X_OK)]
    if len(candidates) != 1:
        raise ValueError(f"Expected one {name} executable in {directory}: {candidates}")
    return candidates[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path, help="Before target's release/deps directory")
    parser.add_argument("after", type=Path, help="After target's release/deps directory")
    parser.add_argument("output", type=Path, help="New output directory; never overwritten")
    args = parser.parse_args()
    programs = {version: {name: executable(directory, name) for name in
                ("game_queries", "cursor_queries", "index_updates")}
                for version, directory in [("before", args.before), ("after", args.after)]}
    args.output.mkdir(parents=False, exist_ok=False)
    metadata = {version: {name: {"path": str(p.resolve()), "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}
                          for name, p in binaries.items()} for version, binaries in programs.items()}
    metadata["conditions"] = {"rows_per_table": 100000, "samples": 2000,
                              "repeats": 5, "area_spans": [1, 2, 8, 32, 128],
                              "platform": sys.platform, "resource_scope": "whole query process"}
    (args.output / "executables.json").write_text(json.dumps(metadata, indent=2) + "\n")

    def run(version, name, arguments, stem, resources=False):
        command = [str(programs[version][name].resolve()), *arguments]
        if resources and sys.platform == "darwin":
            command = ["/usr/bin/time", "-l", *command]
        with (args.output / (stem + ".txt")).open("w") as out:
            if resources:
                with (args.output / (stem + "-resources.txt")).open("w") as err:
                    subprocess.run(command, stdout=out, stderr=err, check=True)
            else:
                subprocess.run(command, stdout=out, check=True)

    for repeat in range(1, 6):
        order = ["before", "after"] if repeat % 2 else ["after", "before"]
        spans = [1, 2, 8, 32, 128] if repeat % 2 else [128, 32, 8, 2, 1]
        for span in spans:
            for version in order:
                arguments = ["100000", "2000", str(span)]
                if repeat % 2 == 0:
                    arguments.append("--sqlite-first")
                run(version, "game_queries", arguments,
                    f"{version}-areas{span}-{repeat}", resources=True)
            print(f"Paired repeat {repeat}, areas {span}: complete rows verified", flush=True)
        for kind, name, arguments in [
            ("cursor", "cursor_queries", ["100000", "128"]),
            ("write", "index_updates", ["100000", "128"]),
        ]:
            for version in order:
                actual = arguments.copy()
                if kind == "cursor" and repeat % 2 == 0:
                    actual.append("--sqlite-first")
                run(version, name, actual, f"{version}-{kind}-{repeat}")
            print(f"Paired repeat {repeat}, {kind}: complete state verified", flush=True)


if __name__ == "__main__":
    main()

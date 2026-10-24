#!/usr/bin/env python3
"""Run each test matching a filter in a process of its own.

A test that passes only because another test ran first in the same process
(registered a plugin, set a variable, left a file) fails here: each gets a
fresh process with nothing registered. The whole filtered set then runs
once more in one process, in the usual way.

    python3 scripts/each-test-alone.py <filter> -- <cargo test arguments>

e.g. `python3 scripts/each-test-alone.py config_wizard -- -p voice-pipes --lib
--features batteries,text-grpc`. The cargo arguments must select one test
binary.
"""

import json
import pathlib
import subprocess
import sys


def main():
    if "--" not in sys.argv:
        sys.exit(__doc__)
    split = sys.argv.index("--")
    (filter_,), cargo_args = sys.argv[1:split], sys.argv[split + 1:]

    built = subprocess.run(
        ["cargo", "test", "--no-run", "--message-format=json", *cargo_args],
        check=True, capture_output=True, text=True,
    )
    binaries = [
        (msg["executable"], pathlib.Path(msg["manifest_path"]).parent)
        for msg in map(json.loads, built.stdout.splitlines())
        if msg.get("reason") == "compiler-artifact"
        and msg.get("profile", {}).get("test")
        and msg.get("executable")
    ]
    if len(binaries) != 1:
        sys.exit(f"the cargo arguments select {len(binaries)} test binaries, not one: {binaries}")
    # Cargo runs a test binary from its package's directory; so does this.
    binary, package_dir = binaries[0]

    listed = subprocess.run(
        [binary, "--list", filter_], check=True, capture_output=True, text=True, cwd=package_dir,
    ).stdout
    tests = [line[: -len(": test")] for line in listed.splitlines() if line.endswith(": test")]
    if not tests:
        sys.exit(f"no test matches {filter_!r}")

    failed = []
    for test in tests:
        run = subprocess.run(
            [binary, "--exact", test, "-q"], capture_output=True, text=True, cwd=package_dir,
        )
        if run.returncode != 0:
            failed.append(test)
            print(f"FAILS ALONE: {test}\n{run.stdout}{run.stderr}", flush=True)
    together = subprocess.run([binary, filter_, "-q"], cwd=package_dir)
    print(f"{len(tests)} tests run one per process, {len(failed)} failed")
    if failed or together.returncode != 0:
        sys.exit(1)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""The numbers a plugin extraction records before and after (plan §5, P5).

    python scripts/p5-measure.py                       # crate counts only
    python scripts/p5-measure.py --build               # + cold release build
    python scripts/p5-measure.py --build --check CONFIG --label "after WS-9"

Prints a Markdown block to paste into the plan's findings:

* crates per build shape, the same six shapes `dep-budget.py` holds to its
  budget;
* with `--build`, the wall time of a cold `cargo build --release
  --timings` of the CLI with its default features, in a target directory
  of its own so no earlier artifact is reused and no running binary is
  locked (removed afterwards unless `--keep-target`), and the size of the
  binary it produced;
* with `--check CONFIG`, the component check's summary for that pipeline,
  run with the binary just built (it reports each component's latency).

Run it on the commit before a move and on the move itself, on the same
machine and otherwise idle; `--source DIR` measures a checkout elsewhere
(a `git worktree` of the earlier commit, say) while this script and its
shapes stay the current ones.
"""

import argparse
import importlib.util
import json
import pathlib
import shutil
import subprocess
import sys
import time

REPO = pathlib.Path(__file__).resolve().parent.parent


def dep_budget():
    """The shapes and counter `dep-budget.py` defines, so both agree."""
    spec = importlib.util.spec_from_file_location("dep_budget", REPO / "scripts" / "dep-budget.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def crate_counts(source):
    budget = dep_budget()
    budget.REPO = source
    return {name: budget.count(args) for name, args in budget.shapes().items()}


def cold_build(source, target_dir):
    """Wall time of a release build from nothing, and the binary's size."""
    if target_dir.exists():
        shutil.rmtree(target_dir)
    started = time.monotonic()
    subprocess.run(
        ["cargo", "build", "--release", "-p", "voice-pipes-cli", "--timings",
         "--target-dir", str(target_dir)],
        cwd=source, check=True,
    )
    seconds = time.monotonic() - started
    exe = "voice-pipes.exe" if sys.platform == "win32" else "voice-pipes"
    binary = target_dir / "release" / exe
    return seconds, binary, binary.stat().st_size


def component_check(binary, config, source):
    """The check's summary: one row per component, then the verdict line,
    the last two blank-separated blocks of its output."""
    out = subprocess.run(
        [str(binary), "--config", str(config), "check"],
        cwd=source / "voice-pipes", capture_output=True, text=True,
    )
    blocks, block = [], []
    for line in out.stdout.splitlines():
        if line.strip():
            block.append(line.rstrip())
        elif block:
            blocks.append(block)
            block = []
    if block:
        blocks.append(block)
    summary = [line for b in blocks[-2:] for line in b]
    if out.returncode != 0:
        summary.append(f"(check exited {out.returncode}: {out.stderr.strip()[-300:]})")
    return summary


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--label", default="", help="what this measurement is of (\"before WS-9\")")
    ap.add_argument("--source", type=pathlib.Path, default=REPO, help="checkout to measure")
    ap.add_argument("--build", action="store_true", help="also time a cold release build")
    ap.add_argument("--check", type=pathlib.Path, help="pipeline config for the component check")
    ap.add_argument("--target-dir", type=pathlib.Path, default=None,
                    help="where the cold build goes (default: <source>/target/p5)")
    ap.add_argument("--json", type=pathlib.Path, help="also write the numbers here")
    ap.add_argument("--keep-target", action="store_true",
                    help="keep the cold build's target directory (it is removed by default)")
    opts = ap.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")
    source = opts.source.resolve()

    commit = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=source,
                            capture_output=True, text=True).stdout.strip()
    result = {"label": opts.label, "commit": commit, "crates": crate_counts(source)}
    if opts.build or opts.check:
        target_dir = (opts.target_dir or source / "target" / "p5").resolve()
        seconds, binary, size = cold_build(source, target_dir)
        result["cold_build_seconds"] = round(seconds)
        result["binary_bytes"] = size
        if opts.check:
            result["check"] = component_check(binary, opts.check.resolve(), source)
        if not opts.keep_target:
            shutil.rmtree(target_dir, ignore_errors=True)

    print(f"#### P5 — {opts.label or 'measurement'} ({commit})\n")
    print("| Shape | Crates |\n|---|---|")
    for name, n in result["crates"].items():
        print(f"| {name} | {n} |")
    if "cold_build_seconds" in result:
        s = result["cold_build_seconds"]
        print(f"\nCold release build of the CLI (defaults): {s // 60} min {s % 60} s; "
              f"binary {result['binary_bytes'] / 1_048_576:.1f} MiB.")
    if "check" in result:
        print("\nComponent check:\n\n```")
        print("\n".join(result["check"]))
        print("```")
    if opts.json:
        opts.json.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())

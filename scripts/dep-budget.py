#!/usr/bin/env python3
"""How many crates each build shape pulls in, held to a budget.

The engine is meant to be a runtime an adopter can take without any
provider; its crate count with no features is the number that says whether
that is still true. This script counts the unique `name version` pairs of
each shape's normal dependency graph for Linux (the images' platform) and
fails if the bare engine grows past its recorded budget plus the slack.

    python3 scripts/dep-budget.py            # check (CI)
    python3 scripts/dep-budget.py --update   # record today's counts as the budget

Each shape is resolved with `cargo tree -p <crate>`, which, unlike a
workspace-wide resolution, does not unify features with other members.
Counts go to the GitHub job summary when `GITHUB_STEP_SUMMARY` is set.
"""

import argparse
import json
import os
import pathlib
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
BUDGET = REPO / "scripts" / "dep-budget.json"
TARGET = "x86_64-unknown-linux-gnu"
# Growth the bare engine may take before CI fails, as a fraction of the budget.
SLACK = 0.05


def image_features(which):
    out = subprocess.run(
        [sys.executable, str(REPO / "scripts" / "image-features.py"), *which],
        check=True, capture_output=True, text=True,
    )
    return out.stdout.strip()


def shapes():
    return {
        "bare engine": ["-p", "voice-pipes", "--no-default-features"],
        "engine batteries": ["-p", "voice-pipes", "--features", "batteries"],
        "CLI, hosted providers only": [
            "-p", "voice-pipes-cli", "--no-default-features",
            "--features", "stt-assemblyai,llm-openai-compat,tts-elevenlabs",
        ],
        "CLI defaults": ["-p", "voice-pipes-cli"],
        "CLI, CPU image": ["-p", "voice-pipes-cli", "--features", image_features(["cli"])],
        "CLI, GPU image": ["-p", "voice-pipes-cli", "--features", image_features(["cli", "--gpu"])],
    }


def count(args):
    out = subprocess.run(
        ["cargo", "tree", *args, "-e", "normal", "--target", TARGET, "--prefix", "none"],
        cwd=REPO, check=True, capture_output=True, text=True,
    )
    return len({" ".join(line.split()[:2]) for line in out.stdout.splitlines() if line.strip()})


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--update", action="store_true", help="record today's counts as the budget")
    opts = ap.parse_args()

    counts = {name: count(args) for name, args in shapes().items()}
    if opts.update:
        BUDGET.write_text(json.dumps({"target": TARGET, "counts": counts}, indent=2) + "\n", encoding="utf-8")
        print(f"recorded {BUDGET.relative_to(REPO)}")

    budget = json.loads(BUDGET.read_text(encoding="utf-8"))["counts"]
    rows = []
    for name, n in counts.items():
        was = budget.get(name)
        delta = "" if was is None else f"{n - was:+d}"
        rows.append((name, n, was, delta))
    width = max(len(r[0]) for r in rows)
    for name, n, was, delta in rows:
        print(f"{name:<{width}}  {n:>4}  (budget {was}, {delta or 'new'})")

    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(f"### Crates per build shape ({TARGET})\n\n| Shape | Crates | Budget |\n|---|---|---|\n")
            for name, n, was, _ in rows:
                f.write(f"| {name} | {n} | {was} |\n")

    limit = int(budget["bare engine"] * (1 + SLACK))
    if counts["bare engine"] > limit:
        print(
            f"FAIL: the bare engine pulls {counts['bare engine']} crates, over its budget of "
            f"{budget['bare engine']} + {int(SLACK * 100)}% ({limit}). A new always-on dependency "
            "belongs behind a feature or in a provider crate; if it truly belongs in every "
            "pipeline, record the new budget with --update in the same change.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

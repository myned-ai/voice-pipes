#!/usr/bin/env python3
"""Print the Cargo features the worker images are built with, split by crate.

The images build `voice-pipes-cli` with the `FEATURES=` build arguments in
`voice-pipes/deploy/docker/docker-compose.yml`. CI compiles exactly that set,
read from the compose file rather than copied, so a feature added to an image
is linted and tested from the same commit, and code that only an image
compiles (a provider behind a feature CI never turns on) cannot rot unseen.

    python3 scripts/image-features.py            # cli=... and engine=... lines
    python3 scripts/image-features.py cli        # the CLI crate's list, comma-separated
    python3 scripts/image-features.py engine     # the part that is the engine's own features
    python3 scripts/image-features.py --gpu ...  # the CUDA image's list instead

The workers share one list, defined once in the compose file (a YAML anchor)
and unioned with the smaller lists of the other images. The CUDA image builds
that list with `GPU_FEATURES` (`cuda,ort-cuda`, from Dockerfile.cuda) on top.
Features that exist only on the CLI crate -- plugin recognisers -- are left
out of the engine list, since `-p voice-pipes` does not know them.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
COMPOSE = REPO / "voice-pipes" / "deploy" / "docker" / "docker-compose.yml"
ENGINE_TOML = REPO / "voice-pipes" / "Cargo.toml"
CLI_TOML = REPO / "voice-pipes-cli" / "Cargo.toml"


DOCKERFILE_CUDA = REPO / "voice-pipes" / "Dockerfile.cuda"


def gpu_switches() -> list[str]:
    m = re.search(r'^ARG GPU_FEATURES="([^"]*)"', DOCKERFILE_CUDA.read_text(encoding="utf-8"), re.M)
    if not m:
        sys.exit(f"no ARG GPU_FEATURES in {DOCKERFILE_CUDA}")
    return [f.strip() for f in m.group(1).split(",") if f.strip()]


def image_features(gpu: bool) -> list[str]:
    # A list item `- FEATURES=...`, or the anchored one `- &name FEATURES=...`;
    # the aliases (`- *name`) repeat it.
    lines = re.findall(r"^\s*-\s*(?:&\S+\s+)?FEATURES=(.*)$", COMPOSE.read_text(encoding="utf-8"), re.M)
    if not lines:
        sys.exit(f"no FEATURES= build args in {COMPOSE}")
    seen: dict[str, None] = {}
    for f in (gpu_switches() if gpu else []) + [f for line in lines for f in line.split(",")]:
        if f.strip():
            seen[f.strip()] = None
    return list(seen)


def engine_features() -> set[str]:
    return set(tomllib.loads(ENGINE_TOML.read_text(encoding="utf-8")).get("features", {}))


def cli_defaults() -> list[str]:
    return tomllib.loads(CLI_TOML.read_text(encoding="utf-8"))["features"].get("default", [])


def main(argv: list[str]) -> int:
    gpu = "--gpu" in argv
    argv = [a for a in argv if a != "--gpu"]
    cli = image_features(gpu)
    # The images build the CLI with its default features on top of FEATURES;
    # the engine has none of its own, so its share includes the CLI's.
    engine = [f for f in dict.fromkeys(cli_defaults() + cli) if f in engine_features()]
    if not argv:
        print("cli=" + ",".join(cli))
        print("engine=" + ",".join(engine))
    elif argv[0] == "cli":
        print(",".join(cli))
    elif argv[0] == "engine":
        print(",".join(engine))
    else:
        sys.exit(__doc__)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

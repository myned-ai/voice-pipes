#!/usr/bin/env python3
"""Fail unless two `voice-pipes inspect --format json` listings name the
same providers.

    python3 scripts/compare-inspect.py native.json image.json

CI runs it on a native build with the image's features and on the image
itself, so the image ships exactly the providers that were tested.
"""

import json
import sys


def providers(path):
    with open(path, encoding="utf-8") as f:
        rows = json.load(f)
    return {(row["kind"], row["name"]) for row in rows}


def main(argv):
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    expected, actual = (providers(p) for p in argv)
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    for kind, name in missing:
        print(f"missing from {argv[1]}: {kind} {name}")
    for kind, name in extra:
        print(f"only in {argv[1]}: {kind} {name}")
    if missing or extra:
        return 1
    print(f"{len(expected)} providers, the same in both")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

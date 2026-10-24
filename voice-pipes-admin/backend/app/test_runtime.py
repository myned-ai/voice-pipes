"""The backend runs on the Python it is tested on, from a locked install.

``pip install -r requirements.txt`` on another interpreter, or a list that
pinned only the direct dependencies, installed versions nobody had tested
(a native install on Python 3.14 took whatever starlette and pydantic-core
were newest). The lock pins every package with its hashes; the backend
refuses another minor version unless told otherwise.
"""
from __future__ import annotations

import os
import re
import unittest
from pathlib import Path
from unittest import mock

from app.core import runtime

BACKEND = Path(__file__).resolve().parent.parent


class PythonVersionTests(unittest.TestCase):
    def test_the_tested_version_has_no_problem(self) -> None:
        self.assertIsNone(runtime.python_problem((3, 12, 0)))
        self.assertIsNone(runtime.python_problem((3, 12, 11, "final", 0)))

    def test_another_version_is_named_with_the_way_out(self) -> None:
        for version in [(3, 11, 9), (3, 13, 1), (3, 14, 0)]:
            problem = runtime.python_problem(version)
            self.assertIsNotNone(problem)
            assert problem is not None
            self.assertIn("Python 3.12", problem)
            self.assertIn(".".join(map(str, version)), problem)
            self.assertIn(runtime.ALLOW_ENV, problem)

    def test_an_untested_version_exits_unless_allowed(self) -> None:
        with mock.patch.object(runtime.sys, "version_info", (3, 14, 0)):
            with mock.patch.dict(os.environ, {runtime.ALLOW_ENV: ""}):
                with self.assertRaises(SystemExit):
                    runtime.require_supported_python()
            with mock.patch.dict(os.environ, {runtime.ALLOW_ENV: "1"}):
                with self.assertLogs("admin.runtime", "WARNING"):
                    runtime.require_supported_python()


def _requirements(path: Path) -> dict[str, tuple[str, str]]:
    """``name -> (pin, the requirement's whole entry)`` for a lock file."""
    entries: dict[str, tuple[str, str]] = {}
    text = path.read_text(encoding="utf-8")
    for block in re.split(r"\n(?=[A-Za-z0-9])", text):
        first = block.strip().splitlines()[0] if block.strip() else ""
        if not first or first.startswith(("#", "-")):
            continue
        m = re.match(r"^([A-Za-z0-9._-]+)(\S*)", first)
        assert m, first
        entries[m.group(1).lower()] = (m.group(2), block)
    return entries


class LockFileTests(unittest.TestCase):
    def test_every_package_is_pinned_exactly_with_hashes(self) -> None:
        for lock in ["requirements.txt", "requirements-dev.txt"]:
            entries = _requirements(BACKEND / lock)
            self.assertTrue(entries, lock)
            for name, (pin, block) in entries.items():
                self.assertTrue(pin.startswith("=="), f"{lock}: {name} is not pinned: {pin!r}")
                self.assertIn("--hash=sha256:", block, f"{lock}: {name} has no hash")

    def test_the_lock_holds_every_direct_dependency_at_its_pin(self) -> None:
        locked = _requirements(BACKEND / "requirements.txt")
        for line in (BACKEND / "requirements.in").read_text(encoding="utf-8").splitlines():
            line = line.split("#")[0].strip()
            if not line:
                continue
            name, _, version = line.partition("==")
            self.assertIn(name.lower(), locked, f"{name} is in requirements.in but not locked")
            if version:
                self.assertEqual(locked[name.lower()][0], f"=={version}", name)

    def test_the_image_runs_the_tested_python_from_the_lock(self) -> None:
        dockerfile = (BACKEND / "Dockerfile").read_text(encoding="utf-8")
        want = ".".join(map(str, runtime.SUPPORTED_PYTHON))
        self.assertRegex(dockerfile, rf"(?m)^FROM python:{re.escape(want)}[-.]")
        self.assertIn("--require-hashes -r requirements.txt", dockerfile)


if __name__ == "__main__":
    unittest.main()

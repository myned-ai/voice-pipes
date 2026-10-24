#!/usr/bin/env python3
"""Check the deploy compose file as Docker resolves it.

Two properties, read from `docker compose config --format json`:

* No two services that start together publish the same host port. "Start
  together" is a plain `up` (the services without a profile) and a plain
  `up` with each profile added, less the services that profile replaces.
* A model-server sidecar and a worker's debug API publish on loopback only:
  neither authenticates its callers on that port the way the worker's
  transport does.

Takes another compose file as its one argument. Exits 1 and names each
problem; needs Docker with the Compose plugin.
"""

from __future__ import annotations

import json
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

COMPOSE = Path(__file__).resolve().parent.parent / "voice-pipes/deploy/docker/docker-compose.yml"

# A profile that brings in another build of a default service: started with
# the service named (`up orchestrator worker-custom-gpu`), never beside it.
REPLACES = {"gpu": {"worker-custom"}}

# Services whose every published port must be loopback-only.
SIDECARS = {"llama-server", "pocket-tts", "vibevoice"}

# The container port of a worker's debug API.
DEBUG_PORT = 8085


def resolved(compose: Path) -> dict:
    out = subprocess.run(
        ["docker", "compose", "-f", str(compose), "--profile", "*", "config", "--format", "json"],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(out.stdout)["services"]


def host_ports(service: dict) -> list[tuple[str, str, int]]:
    return [
        (p.get("host_ip", ""), str(p.get("published", "")), int(p["target"]))
        for p in service.get("ports", [])
        if p.get("published")
    ]


def collisions(services: dict, names: set[str], label: str) -> list[str]:
    users: dict[str, list[str]] = defaultdict(list)
    for name in sorted(names):
        for _, published, _ in host_ports(services[name]):
            users[published].append(name)
    return [
        f"{label}: host port {port} is published by {', '.join(names)}"
        for port, names in sorted(users.items())
        if len(names) > 1
    ]


def exposure(services: dict) -> list[str]:
    problems = []
    for name, service in sorted(services.items()):
        for host_ip, published, target in host_ports(service):
            if host_ip == "127.0.0.1":
                continue
            if name in SIDECARS:
                problems.append(f"{name} publishes {published} on all interfaces; bind it to 127.0.0.1")
            elif target == DEBUG_PORT:
                problems.append(f"{name}'s debug API ({published}) is on all interfaces; bind it to 127.0.0.1")
    return problems


def problems(services: dict) -> list[str]:
    default = {n for n, s in services.items() if not s.get("profiles")}
    found = collisions(services, default, "plain `up`")
    profiles = sorted({p for s in services.values() for p in s.get("profiles", [])})
    for profile in profiles:
        members = {n for n, s in services.items() if profile in s.get("profiles", [])}
        together = (default - REPLACES.get(profile, set())) | members
        found += collisions(services, together, f"`--profile {profile} up`")
    return found + exposure(services)


def self_test() -> None:
    def svc(*ports: str, profiles: list[str] | None = None) -> dict:
        parsed = []
        for spec in ports:
            parts = spec.split(":")
            ip = parts[0] if len(parts) == 3 else ""
            parsed.append({"host_ip": ip, "published": parts[-2], "target": int(parts[-1])})
        return {"ports": parsed, **({"profiles": profiles} if profiles else {})}

    clash = {"a": svc("8800:8800"), "b": svc("8800:8800")}
    assert problems(clash), "two default services on one port"
    clash = {"a": svc("127.0.0.1:8085:8085"), "b": svc("127.0.0.1:8085:8085", profiles=["x"])}
    assert problems(clash), "a profile's service on a default service's port"
    swap = {"worker-custom": svc("8800:8800"), "g": svc("8800:8800", profiles=["gpu"])}
    assert not problems(swap), "a declared replacement"
    assert problems({"vibevoice": svc("8091:8091")}), "a sidecar on all interfaces"
    assert problems({"w": svc("8086:8085")}), "a debug API on all interfaces"
    assert not problems({"w": svc("8800:8800", "127.0.0.1:8086:8085")}), "a clean worker"


def main() -> int:
    self_test()
    compose = Path(sys.argv[1]) if len(sys.argv) > 1 else COMPOSE
    found = problems(resolved(compose))
    for p in found:
        print(f"compose-check: {p}", file=sys.stderr)
    if not found:
        print("compose-check: ok")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())

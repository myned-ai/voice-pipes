#!/usr/bin/env python3
"""idle_soak -- what an idle worker's first turn costs, and why.

A worker that sits idle can have its models swapped out (Linux swap) or, on
Docker Desktop, its whole VM's memory trimmed by the host. The next caller
then waits while it all comes back. This script measures that: for each idle
gap it leaves the worker untouched, then sends one probe session ("after
idle") and a second one straight after ("warm"), and records per stage

    stt endpointing, llm ttft, tts ttfb (and tts internal ttfb), first audio

plus the worker process's swapped memory and major page faults, read through
the Docker daemon (a privileged helper container in the host's PID space).
Major faults that climb during the first turn mean the Linux side paged; a
slow first turn with few new faults points below Linux (the VM's host).

Typical use:

    # overnight: gaps of 5, 15, 30, 60 and 120 minutes against one worker
    python scripts/idle_soak.py --url ws://localhost:8807/ws \
        --container docker-worker-english-moonshine-1 --idle-min 5,15,30,60,120 \
        --json soak.jsonl

    # in minutes: swap this worker out instead of waiting for it to happen
    python scripts/idle_soak.py --url ws://localhost:8807/ws \
        --container docker-worker-english-moonshine-1 --idle-min 0,0,0 \
        --squeeze-mb 200

`--squeeze-mb` needs free swap for the worker's pages and refuses to run
without it. `--pressure-mb` allocates memory in a throwaway container
instead; the kernel then swaps whatever was least recently used, which is
only this worker once it has sat idle for a while.

Nothing else may talk to the worker during a gap, or the gap is not idle.
Needs `pip install websockets` and a Docker CLI for the memory figures.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import s2s_probe  # noqa: E402


def proc_memory(container: str | None) -> dict:
    """Swapped/resident memory (MB) and major faults of the container's
    main process, or {} when Docker is not reachable."""
    if not container:
        return {}
    try:
        pid = subprocess.run(
            ["docker", "inspect", "-f", "{{.State.Pid}}", container],
            capture_output=True, text=True, check=True, timeout=20,
        ).stdout.strip()
        out = subprocess.run(
            ["docker", "run", "--rm", "--privileged", "--pid=host", "alpine", "sh", "-c",
             f"awk '{{print $12}}' /proc/{pid}/stat; "
             f"awk '/VmSwap|VmRSS/{{print $2}}' /proc/{pid}/status"],
            capture_output=True, text=True, check=True, timeout=60,
        ).stdout.split()
        majflt, rss_kb, swap_kb = (int(x) for x in out[:3])
        return {"majflt": majflt, "rss_mb": rss_kb // 1024, "swap_mb": swap_kb // 1024}
    except (subprocess.SubprocessError, ValueError, OSError):
        return {}


def apply_pressure(mb: int) -> None:
    """Allocate `mb` of memory in a throwaway container and release it, so
    the kernel swaps idle processes out the way a long idle spell under a
    busy neighbour would."""
    # `tail` buffers /dev/zero looking for a newline: a memory hog in one
    # line with nothing to install.
    subprocess.run(
        ["docker", "run", "--rm", "alpine", "sh", "-c",
         f"head -c {mb}m /dev/zero | tail > /dev/null"],
        capture_output=True, timeout=600,
    )


def squeeze(container: str, mb: int, hold_s: float = 5.0) -> None:
    """Swap the container's own memory out now: cap it at `mb` for a few
    seconds (swap allowed), then lift the cap. What hours of idling under a
    busy neighbour do to it, in seconds and aimed at this worker alone. A
    container that may not swap (memory-swap equal to memory) cannot be
    squeezed below its working set without being killed, so it is skipped."""
    info = subprocess.run(
        ["docker", "inspect", "-f", "{{.HostConfig.Memory}} {{.HostConfig.MemorySwap}}", container],
        capture_output=True, text=True, check=True, timeout=20,
    ).stdout.split()
    memory, memory_swap = int(info[0]), int(info[1])
    if memory and memory_swap == memory:
        print(f"   ({container} may not swap; not squeezed)", flush=True)
        return
    # Squeezed with too little free swap to take its pages, the kernel kills
    # the worker instead of swapping it.
    free_swap_mb = int(subprocess.run(
        ["docker", "run", "--rm", "--privileged", "--pid=host", "alpine", "sh", "-c",
         "awk '/SwapFree/{print $2}' /proc/meminfo"],
        capture_output=True, text=True, check=True, timeout=60,
    ).stdout.strip() or 0) // 1024
    rss_mb = proc_memory(container).get("rss_mb", 0)
    if free_swap_mb < rss_mb - mb + 256:
        raise SystemExit(
            f"only {free_swap_mb} MB of swap free for a {rss_mb} MB worker: squeezing it to "
            f"{mb} MB would get it killed, not swapped. Free some swap first."
        )
    subprocess.run(["docker", "update", "--memory", f"{mb}m", "--memory-swap", "-1", container],
                   capture_output=True, check=True, timeout=60)
    time.sleep(hold_s)
    # Put back what it had: unlimited, or its configured limits.
    restore = (["--memory", str(memory), "--memory-swap", str(memory_swap)] if memory
               else ["--memory", "64g", "--memory-swap", "-1"])
    subprocess.run(["docker", "update", *restore, container], capture_output=True, check=True, timeout=60)


def stages(row: dict) -> dict:
    c = row.get("components") or {}
    stt, llm, tts = c.get("stt", {}), c.get("llm", {}), c.get("tts", {})
    return {
        "first_audio_ms": row.get("first_audio_after_ms"),
        "stt_endpointing_ms": stt.get("endpointing_ms"),
        "llm_ttft_ms": llm.get("ttft_ms"),
        "tts_ttfb_ms": tts.get("ttfb_ms"),
        "tts_internal_ttfb_ms": tts.get("tts_internal_ttfb_ms"),
    }


async def probe(args) -> dict:
    ns = argparse.Namespace(
        url=s2s_probe._with_client_token(args.url, args.token), token=None, wav=args.wav,
        start_s=args.start_s, end_s=args.end_s, lead_s=1.0, trail_s=4.0, chunk_ms=2.667,
        repeat=1, gap_s=0, wait_s=20.0, json=None, verbose=False, keep_snapshot=True,
    )
    return stages(await s2s_probe.run_once(ns, 1))


def fmt(v) -> str:
    return "-" if v is None else f"{v:.0f}"


async def main(args) -> None:
    gaps = [float(x) for x in args.idle_min.split(",") if x.strip()]
    cols = ["first_audio_ms", "stt_endpointing_ms", "llm_ttft_ms", "tts_ttfb_ms", "tts_internal_ttfb_ms"]
    print(f"{'idle':>6} {'run':>6} " + " ".join(f"{c[:-3]:>18}" for c in cols) + f" {'swap':>6} {'+faults':>8}")
    for gap in gaps:
        if gap > 0:
            print(f"-- idle {gap:g} min", flush=True)
            await asyncio.sleep(gap * 60)
        if args.pressure_mb:
            apply_pressure(args.pressure_mb)
        if args.squeeze_mb and args.container:
            squeeze(args.container, args.squeeze_mb)
        for label in ("idle", "warm"):
            before = proc_memory(args.container)
            t = time.time()
            result = await probe(args)
            after = proc_memory(args.container)
            faults = (after.get("majflt", 0) - before.get("majflt", 0)) if before and after else None
            row = {"ts": t, "idle_min": gap, "pressure_mb": args.pressure_mb, "run": label,
                   **result, "swap_mb_before": before.get("swap_mb"), "major_faults": faults}
            print(f"{gap:>6g} {label:>6} " + " ".join(f"{fmt(result[c]):>18}" for c in cols)
                  + f" {fmt(before.get('swap_mb')):>6} {fmt(faults):>8}", flush=True)
            if args.json:
                with open(args.json, "a", encoding="utf-8") as f:
                    f.write(json.dumps(row) + "\n")


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default="ws://localhost:8800/ws")
    ap.add_argument("--token", default=None, help="VP_CLIENT_TOKEN (default: $VP_CLIENT_TOKEN or voice-pipes/.env)")
    ap.add_argument("--container", help="worker container name, for swap and page-fault figures")
    ap.add_argument("--idle-min", default="5,15,30,60,120", help="comma-separated idle gaps in minutes")
    ap.add_argument("--pressure-mb", type=int, default=0,
                    help="allocate this much memory before each 'idle' probe to force swapping")
    ap.add_argument("--squeeze-mb", type=int, default=0,
                    help="swap the worker's own memory out before each 'idle' probe by capping "
                         "its container at this many MB for a few seconds (needs --container)")
    ap.add_argument("--wav", default=str(Path(__file__).resolve().parents[1] / "fixtures" / "hello-how-are-you.wav"))
    ap.add_argument("--start-s", type=float, default=0.0)
    ap.add_argument("--end-s", type=float, default=1.9)
    ap.add_argument("--json", help="append one JSON row per probe to this file")
    asyncio.run(main(ap.parse_args()))

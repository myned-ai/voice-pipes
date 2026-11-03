#!/usr/bin/env python3
"""s2s_probe -- scripted vp caller for realtime (speech-to-speech) latency work.

Replays a spoken segment of a WAV file into a vp worker session exactly the way
the Playground does (``configure`` handshake, 48 kHz mono i16 binary audio
frames, realtime-paced, browser-sized chunks by default) and stamps every hop
on the client side against the *true* end of speech (the last speech sample
put on the wire), which no provider-reported anchor can move:

    speech_end_sent          last sample of the speech segment sent
    state_speaking           server stamped TTFB (`state_changed: speaking`)
    first_audio_binary       first audio frame with samples reached the client
    turn_metrics             the turn snapshot (ttfb_ms, user_speech_end_epoch_ms, ...)

Typical use:

    python scripts/s2s_probe.py --wav captures/<session>/002_mic_raw.wav \
        --start-s 18.2 --end-s 19.5 --repeat 3
    python scripts/s2s_probe.py ... --chunk-ms 20          # uplink batching A/B
    python scripts/s2s_probe.py ... --json runs.jsonl      # machine-readable rows

Requires ``pip install websockets``. The worker must be reachable (default
ws://localhost:8800/ws) and run a `[realtime_provider]` pipeline; cascade
pipelines work too but only the client-side columns are meaningful there.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import struct
import time
import wave

import websockets

MSG_AUDIO = 0x01
FLAG_EOU = 0x01


def _with_client_token(url: str, token: str | None) -> str:
    """The worker's /ws enforces VP_CLIENT_TOKEN when the stack was
    started from the shipped configs. Resolve the token from --token, then
    $VP_CLIENT_TOKEN, then the launcher-generated voice-pipes/.env; append it
    as `?token=`. An open worker ignores the parameter."""
    import os
    import pathlib
    from urllib.parse import quote

    if not token:
        token = os.getenv("VP_CLIENT_TOKEN", "").strip()
    if not token:
        dotenv = pathlib.Path(__file__).resolve().parents[1] / "voice-pipes" / ".env"
        if dotenv.is_file():
            for line in dotenv.read_text(encoding="utf-8").splitlines():
                if line.startswith("VP_CLIENT_TOKEN="):
                    token = line.split("=", 1)[1].strip().strip('"')
    if not token:
        return url
    sep = "&" if "?" in url else "?"
    return f"{url}{sep}token={quote(token, safe='')}"


def now_ms() -> float:
    return time.time() * 1000.0


def load_segment(path: str, start_s: float, end_s: float, lead_s: float, trail_s: float):
    w = wave.open(path)
    rate, ch, sw = w.getframerate(), w.getnchannels(), w.getsampwidth()
    if ch != 1 or sw != 2:
        raise SystemExit(f"{path}: need mono 16-bit PCM (got channels={ch}, width={sw})")
    w.setpos(int(start_s * rate))
    speech = w.readframes(int((end_s - start_s) * rate))
    lead = b"\x00\x00" * int(lead_s * rate)
    trail = b"\x00\x00" * int(trail_s * rate)
    return rate, lead, speech, trail


async def run_once(args, run_idx: int) -> dict:
    rate, lead, speech, trail = load_segment(args.wav, args.start_s, args.end_s, args.lead_s, args.trail_s)
    pcm = lead + speech + trail
    speech_end_byte = len(lead) + len(speech)
    chunk_bytes = max(2, int(rate * args.chunk_ms / 1000.0)) * 2
    log: list[tuple[float, str, dict]] = []
    state: dict = {"first_audio": None, "audio_frames": 0, "audio_samples": 0, "turn_metrics": None,
                   "speech_end_ms": None, "speaking_ms": None}

    def mark(ev: str, **kw):
        log.append((now_ms(), ev, kw))

    async with websockets.connect(_with_client_token(args.url, args.token), max_size=None) as ws:
        await ws.send(json.dumps({"type": "control", "payload": {
            "action": "configure", "sample_rate": rate, "output_sample_rate": 24000,
            "channels": 1, "mode": "voice"}}))
        ready = asyncio.Event()
        done = asyncio.Event()

        async def reader():
            async for msg in ws:
                t = now_ms()
                if isinstance(msg, (bytes, bytearray)):
                    mt, flags, _seq = struct.unpack_from("<BBI", msg, 0)
                    n = (len(msg) - 6) // 2
                    if mt == MSG_AUDIO and n > 0:
                        state["audio_frames"] += 1
                        state["audio_samples"] += n
                        if state["first_audio"] is None:
                            state["first_audio"] = t
                            mark("first_audio_binary", samples=n, ms=n * 1000 // 24000)
                    elif mt == MSG_AUDIO and flags & FLAG_EOU:
                        mark("audio_eou_sentinel")
                    continue
                try:
                    env = json.loads(msg)
                except (TypeError, ValueError):
                    continue
                p = env.get("payload") if isinstance(env, dict) else None
                if not isinstance(p, dict):
                    continue
                action = p.get("action")
                if action == "session_ready":
                    ready.set()
                    mark("session_ready")
                elif action == "state_changed":
                    if p.get("state") == "speaking" and state["speaking_ms"] is None:
                        state["speaking_ms"] = t
                    mark("state_" + str(p.get("state")))
                elif action == "turn_metrics":
                    state["turn_metrics"] = p.get("snapshot") or p
                    mark("turn_metrics")
                    done.set()
                elif action == "error":
                    mark("error", **{k: v for k, v in p.items() if k != "action"})
                elif env.get("type") == "message" and p.get("kind") == "text" and args.verbose:
                    mark("text_msg", role=p.get("role"), text=(p.get("text") or "")[:60])

        rtask = asyncio.create_task(reader())
        await asyncio.wait_for(ready.wait(), 10)
        await asyncio.sleep(0.5)
        t0 = time.perf_counter()
        sent = 0
        seq = 0
        mark("uplink_start", chunk_ms=round(chunk_bytes / 2 / rate * 1000, 3))
        while sent < len(pcm):
            frame = pcm[sent:sent + chunk_bytes]
            await ws.send(struct.pack("<BBI", MSG_AUDIO, 0, seq) + frame)
            seq += 1
            prev, sent = sent, sent + len(frame)
            if prev < speech_end_byte <= sent and state["speech_end_ms"] is None:
                state["speech_end_ms"] = now_ms()
                mark("speech_end_sent")
            delay = (t0 + sent / 2 / rate) - time.perf_counter()
            if delay > 0:
                await asyncio.sleep(delay)
        mark("uplink_done", frames=seq)
        try:
            await asyncio.wait_for(done.wait(), args.wait_s)
        except asyncio.TimeoutError:
            mark("timeout_waiting_turn_metrics")
        await asyncio.sleep(0.3)
        rtask.cancel()

    se = state["speech_end_ms"]
    rt = {}
    snap = state["turn_metrics"] or {}
    for c in snap.get("components") or []:
        if c.get("component_id") in ("realtime", "output"):
            rt.update({k: v for k, v in (c.get("extras") or {}).items()
                       if k in ("ttfb_ms", "first_text_ms", "ttfb_anchor_source", "user_speech_end_epoch_ms",
                                "vp_speech_end_epoch_ms", "provider_endpointing_ms", "audio_output_ms",
                                "emit_rtf", "source_rtf")})
    row = {
        "run": run_idx,
        "chunk_ms": args.chunk_ms,
        "speech_end_epoch_ms": se,
        "anchor_lag_ms": (rt["user_speech_end_epoch_ms"] - se) if se and rt.get("user_speech_end_epoch_ms") else None,
        "speaking_after_ms": (state["speaking_ms"] - se) if se and state["speaking_ms"] else None,
        "first_audio_after_ms": (state["first_audio"] - se) if se and state["first_audio"] else None,
        "server_ttfb_ms": rt.get("ttfb_ms"),
        "first_text_ms": rt.get("first_text_ms"),
        "anchor_source": rt.get("ttfb_anchor_source"),
        "provider_endpointing_ms": rt.get("provider_endpointing_ms"),
        "audio_output_ms": rt.get("audio_output_ms"),
        "emit_rtf": rt.get("emit_rtf"),
        "response": (snap.get("llm_response_text") or "")[:80],
    }
    if getattr(args, "keep_snapshot", False):
        # Every component's extras, for callers that break latency down by
        # stage (scripts/idle_soak.py); not written to --json.
        row["components"] = {
            c.get("component_id"): c.get("extras") or {} for c in snap.get("components") or []
        }
    if args.verbose:
        for t, ev, kw in log:
            rel = f"{t - se:+8.0f}" if se else "        "
            print(f"  {t:.0f} {rel} ms  {ev} {kw if kw else ''}")
    return row


def fmt(v, width=8):
    if v is None:
        return "-".rjust(width)
    if isinstance(v, float):
        return f"{v:.0f}".rjust(width)
    return str(v).rjust(width)


async def main(args):
    rows = []
    print(f"{'run':>3} {'chunk':>6} {'anchor':>8} {'speak':>8} {'audio':>8} {'ttfb':>8} {'text':>8} {'endpt':>8}  source / response")
    for i in range(args.repeat):
        row = await run_once(args, i + 1)
        rows.append(row)
        print(f"{row['run']:>3} {row['chunk_ms']:>6} {fmt(row['anchor_lag_ms'])} {fmt(row['speaking_after_ms'])} "
              f"{fmt(row['first_audio_after_ms'])} {fmt(row['server_ttfb_ms'])} {fmt(row['first_text_ms'])} "
              f"{fmt(row['provider_endpointing_ms'])}  {row['anchor_source'] or '-'} / {row['response']}")
        if args.json:
            with open(args.json, "a", encoding="utf-8") as f:
                f.write(json.dumps(row) + "\n")
        if i + 1 < args.repeat:
            await asyncio.sleep(args.gap_s)
    good = [r["first_audio_after_ms"] for r in rows if r["first_audio_after_ms"] is not None]
    if good:
        good.sort()
        print(f"speech end -> first audio at client: min {good[0]:.0f} ms, "
              f"median {good[len(good) // 2]:.0f} ms, max {good[-1]:.0f} ms (n={len(good)})")
    print("columns are ms after the true end of speech: anchor = provider/vp speech-end stamp, "
          "speak = TTFB stamped (`state: speaking`), audio = first audio frame received")


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default="ws://localhost:8800/ws")
    ap.add_argument("--token", default=None,
                    help="worker client token; default: $VP_CLIENT_TOKEN, else voice-pipes/.env")
    ap.add_argument("--wav", required=True, help="mono 16-bit WAV (a captures/<id>/NNN_mic_raw.wav works)")
    ap.add_argument("--start-s", type=float, required=True, help="speech segment start (s)")
    ap.add_argument("--end-s", type=float, required=True, help="speech segment end (s) = the true end of speech")
    ap.add_argument("--lead-s", type=float, default=1.0, help="silence before the segment (s)")
    ap.add_argument("--trail-s", type=float, default=6.0, help="silence after the segment (s)")
    ap.add_argument("--chunk-ms", type=float, default=2.667, help="uplink chunk (2.667 = browser worklet quantum)")
    ap.add_argument("--repeat", type=int, default=1)
    ap.add_argument("--gap-s", type=float, default=2.0, help="pause between repeats (s)")
    ap.add_argument("--wait-s", type=float, default=12.0, help="max wait for turn_metrics (s)")
    ap.add_argument("--json", help="append one JSON row per run to this file")
    ap.add_argument("--verbose", action="store_true", help="print the full per-run hop log")
    asyncio.run(main(ap.parse_args()))

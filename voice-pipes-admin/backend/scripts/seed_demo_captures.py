"""Generate realistic demo capture sessions so the admin dashboard has data.

Writes session dirs (metadata.json + NNN_turn_resolved.json + events.jsonl) that
the BFF's /api/v1/analytics + session views consume.  Timestamps are spread over
the last few days so the default 7-day window and the time-series charts look
populated.

Usage (from backend/):
    python scripts/seed_demo_captures.py [count] [captures_dir]

Defaults: 24 sessions into ../../voice-pipes/captures
"""
import json
import random
import sys
import uuid
from datetime import UTC, datetime, timedelta
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent.parent  # repo root
DEFAULT_CAPS = ROOT / "voice-pipes" / "captures"

PIPELINES = ["support-en", "sales-en", "voicemail"]
STT = ["deepgram", "parakeet", "assemblyai"]
TTS = ["cartesia", "elevenlabs", "piper"]
LLM = ["openai/gpt-4o-mini", "openai/gpt-4o", "local/llama-3.1-8b"]
VOICES = {"cartesia": "sonic-en", "elevenlabs": "rachel", "piper": "en-us"}


def _w(p: Path, obj):
    p.write_text(json.dumps(obj), encoding="utf-8")


def gen_session(caps: Path, started: datetime, rng: random.Random):
    sid = str(uuid.uuid4())
    d = caps / sid
    d.mkdir(parents=True, exist_ok=True)
    pipe = rng.choice(PIPELINES)
    stt = rng.choice(STT)
    tts = rng.choice(TTS)
    llm = rng.choice(LLM)
    iso = started.isoformat()
    # Business outcome + CSAT — what the call achieved and how the
    # caller rated it.  contained = AI resolved without a human; escalated =
    # handed to an agent; abandoned = caller dropped.
    roll = rng.random()
    outcome = "contained" if roll < 0.62 else ("escalated" if roll < 0.82 else "abandoned")
    csat = None
    if rng.random() < 0.7:
        if outcome == "contained":
            csat = rng.choices([3, 4, 5], weights=[1, 3, 4])[0]
        elif outcome == "escalated":
            csat = rng.choices([2, 3, 4], weights=[2, 3, 2])[0]
        else:
            csat = rng.choices([1, 2, 3], weights=[3, 2, 1])[0]
    _w(d / "metadata.json", {
        "id": sid, "start_time": iso, "started_at": iso,
        "pipeline_name": pipe, "stt_provider": stt, "tts_provider": tts,
        "outcome": outcome, "csat": csat,
    })

    n_turns = rng.choices([1, 2, 3, 4], weights=[3, 4, 2, 1])[0]
    fail = rng.random() < 0.12          # ~12% sessions record an error
    no_turns = rng.random() < 0.05      # ~5% produce no turns at all
    events = []
    pipeline_events = []  # canonical per-turn snapshots (turn_metrics_ready)
    if no_turns:
        (d / "events.jsonl").write_text("", encoding="utf-8")
        return

    for t in range(1, n_turns + 1):
        # latencies with a fat tail so percentiles/SLO breaches are visible
        base = rng.gauss(1400, 350)
        if rng.random() < 0.1:
            base += rng.uniform(1500, 3000)   # occasional slow turn
        total = max(500, int(base))
        ttft = int(total * rng.uniform(0.18, 0.30))
        ttfb = ttft + int(rng.uniform(80, 180))
        endpointing = int(rng.uniform(180, 520))
        # turn_resolved.json carries lifecycle only; canonical latencies live in
        # the per-turn snapshot (single source of truth).
        _w(d / f"{t:03d}_turn_resolved.json", {"utterance_id": t})
        pipeline_events.append({
            "kind": "turn_metrics_ready", "utterance_id": t,
            "metrics": {
                "utterance_id": t, "total_ms": total, "interrupted": False,
                "components": [
                    {"component_id": "stt", "kind": "stt",
                     "extras": {"endpointing_ms": endpointing}},
                    {"component_id": "llm", "kind": "llm",
                     "extras": {"ttft_ms": ttft}},
                    {"component_id": "tts", "kind": "tts",
                     "extras": {"ttfb_ms": ttfb}},
                ],
            },
        })
        out_tok = rng.randint(25, 120)
        gen_ms = int(out_tok / rng.uniform(0.04, 0.08))
        events.append({"event_type": "llm_timing", "payload": {
            "model": llm, "prompt_tokens": rng.randint(60, 400),
            "output_tokens": out_tok, "elapsed_ms": gen_ms,
            "tokens_per_sec": round(out_tok / (gen_ms / 1000), 1),
            "first_token_ms": ttft}})
        events.append({"event_type": "stt_usage", "payload": {
            "provider": stt, "speech_model": "nova-3" if stt == "deepgram" else stt,
            "audio_duration_seconds": round(rng.uniform(2, 8), 1),
            "session_duration_seconds": round(rng.uniform(4, 12), 1)}})
        events.append({"event_type": "tts_usage", "payload": {
            "provider": tts, "voice": VOICES[tts],
            "chars": rng.randint(40, 220), "audio_duration_s": round(rng.uniform(1.5, 6), 1)}})
        # client timings -> client TTFA + occasional underrun
        se = 1000 * t
        events.append({"event_type": "client_timing", "payload": {
            "utterance_id": t, "event": "speech_end", "client_timestamp_ms": se}})
        events.append({"event_type": "client_timing", "payload": {
            "utterance_id": t, "event": "playback_start",
            "client_timestamp_ms": se + ttfb + int(rng.uniform(20, 120))}})
        if rng.random() < 0.08:
            events.append({"event_type": "client_timing", "payload": {
                "utterance_id": t, "event": "underrun", "client_timestamp_ms": se + 500}})
        if rng.random() < 0.15:
            events.append({"event_type": "barge_in_rtt", "payload": {
                "utterance_id": t, "rtt_ms": int(rng.uniform(120, 480))}})

    if fail:
        comp = rng.choice(["stt", "llm", "tts"])
        events.append({"event_type": f"{comp}_error", "payload": {
            "utterance_id": rng.randint(1, n_turns), "message": f"{comp} upstream timeout"}})

    (d / "events.jsonl").write_text(
        "".join(json.dumps(e) + "\n" for e in events), encoding="utf-8")
    (d / "pipeline_events.jsonl").write_text(
        "".join(json.dumps(e) + "\n" for e in pipeline_events), encoding="utf-8")


def main():
    count = int(sys.argv[1]) if len(sys.argv) > 1 else 24
    caps = Path(sys.argv[2]) if len(sys.argv) > 2 else DEFAULT_CAPS
    caps.mkdir(parents=True, exist_ok=True)
    rng = random.Random(42)
    now = datetime.now(UTC)
    for _ in range(count):
        # spread across the last ~5 days, clustered in business hours
        started = now - timedelta(
            days=rng.uniform(0, 5), hours=rng.uniform(0, 8), minutes=rng.uniform(0, 59))
        gen_session(caps, started, rng)
    print(f"wrote {count} demo sessions to {caps}")


if __name__ == "__main__":
    main()

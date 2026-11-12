import json
import logging
import re
import wave as _wave
from pathlib import Path

from fastapi import HTTPException

from ..core import config
from ..core.config import _CAPTURES_DIR_FALLBACK, _UTT_FILE_RE, _resolve_captures_dir
from .jsonl import count_unreadable_lines, parse_jsonl_line
from .speech_bounds import speech_bounds

log = logging.getLogger("admin.captures")

# ── Helper: read local capture artifacts for a session ───────

_TTS_FRAG_RE = re.compile(r"^tts_frag_(\d+)$")
_TTS_FRAG_SPOKEN_RE = re.compile(r"^tts_frag_(\d+)_spoken$")
# The microphone audio of speech that made no turn, kept under the id the
# speech would have become and the reason it was dropped; a second drop
# under the same id carries its attempt number.
_DROPPED_FILE_RE = re.compile(
    r"^(?P<utt>\d+)_dropped_(?P<reason>[a-z_]+?)(?:_(?P<attempt>\d+))?_mic_raw\.wav$"
)
_SAFE_SESSION_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]*$")


def _resolve_session_dir(session_id: str, tenant_id: str | None = None) -> Path:
    """Return a validated session directory under config.CAPTURES_DIR.

    When ``tenant_id`` is provided, resolution is strictly scoped to that tenant's
    partition (captures/<tenant_id>/<session_id>). When ``tenant_id`` is None, the
    flat directory is tried first, followed by tenant partition discovery.
    """
    if config.CAPTURES_DIR == _CAPTURES_DIR_FALLBACK:
        _resolve_captures_dir()

    if not _SAFE_SESSION_ID_RE.fullmatch(session_id):
        raise HTTPException(status_code=400, detail="Invalid session id")
    captures_root = config.CAPTURES_DIR.resolve()

    if isinstance(tenant_id, str):
        if not _SAFE_SESSION_ID_RE.fullmatch(tenant_id):
            raise HTTPException(status_code=400, detail="Invalid tenant id")
        session_dir = (captures_root / tenant_id / session_id).resolve()
        if not session_dir.is_relative_to(captures_root):
            raise HTTPException(status_code=400, detail="Invalid session id")
        return session_dir

    flat_dir = (captures_root / session_id).resolve()
    if flat_dir.exists() and flat_dir.is_relative_to(captures_root):
        return flat_dir

    if captures_root.exists():
        try:
            for child in captures_root.iterdir():
                if child.is_dir() and _SAFE_SESSION_ID_RE.fullmatch(child.name):
                    candidate = (child / session_id).resolve()
                    if candidate.exists() and candidate.is_relative_to(captures_root):
                        return candidate
        except OSError:
            pass

    return flat_dir


def _read_session_metadata(session_id: str, tenant_id: str | None = None) -> dict:
    """Return the session's ``metadata.json``, or ``{}`` when it is absent,
    unreadable or torn (the session is then shown without metadata)."""
    dir_path = (
        _resolve_session_dir(session_id, tenant_id=tenant_id)
        if isinstance(tenant_id, str)
        else _resolve_session_dir(session_id)
    )
    meta_path = dir_path / "metadata.json"
    if meta_path.exists():
        try:
            with open(meta_path, encoding="utf-8") as f:
                return json.load(f)
        except (OSError, ValueError):
            pass
    return {}


def _wav_duration_ms(path: Path) -> float:
    """Return duration of a WAV file in milliseconds using the stdlib wave module."""
    try:
        with _wave.open(str(path), "rb") as w:
            return (w.getnframes() / w.getframerate()) * 1000.0
    except (_wave.Error, EOFError, OSError, ZeroDivisionError):
        return 0.0


def _normalize_llm_payload(p: dict) -> dict:
    """Python mirror of frontend `normalizeLlmTiming` (utils.ts).

    Reconciles the two payload variants:
      - Rust worker: `elapsed_ms` + nested `usage.{prompt,completion}_tokens`
      - Python/OpenAI: `total_ms` + flat `prompt_tokens`/`output_tokens`
    """
    raw = p or {}
    usage = raw.get("usage") or {}
    prompt_tokens = raw.get("prompt_tokens") or usage.get("prompt_tokens")
    output_tokens = (
        raw.get("output_tokens")
        or usage.get("completion_tokens")
        or usage.get("output_tokens")
        or raw.get("tokens")
    )
    total_ms = raw.get("total_ms") or raw.get("elapsed_ms")
    decode_ms = _num(raw.get("decode_ms"))
    generate_ms = _num(raw.get("generate_ms") or raw.get("generation_ms") or raw.get("elapsed_ms"))
    effective_decode_ms = decode_ms if decode_ms is not None else generate_ms
    tokens_per_sec = _num(raw.get("tokens_per_sec"))
    # One definition: (output_tokens - 1) / (decode_ms / 1000.0) when output_tokens >= 2
    # and decode_ms > 0 (inverse of OTel gen_ai.server.time_per_output_token); null when < 2 tokens.
    if tokens_per_sec is None and output_tokens is not None and effective_decode_ms and effective_decode_ms > 0:
        if output_tokens >= 2:
            tokens_per_sec = round(((output_tokens - 1) / (effective_decode_ms / 1000.0)) * 10) / 10
        else:
            tokens_per_sec = None
    return {
        "prompt_tokens": prompt_tokens,
        "output_tokens": output_tokens,
        "total_ms": total_ms,
        "generate_ms": generate_ms,
        "decode_ms": decode_ms,
        "tokens_per_sec": tokens_per_sec,
        # Extras used by report-for-llm; harmless when consumers ignore them.
        "first_token_ms": raw.get("first_token_ms"),
        "prompt_ingest_ms": _num(raw.get("prompt_ingest_ms")),
        "connect_ms": raw.get("connect_ms"),
        "model": raw.get("model"),
        "provider": raw.get("provider"),
        "stream": raw.get("stream"),
    }


def _num(v) -> float | None:
    """Coerce to float, rejecting bools and non-numerics."""
    if isinstance(v, (int, float)) and not isinstance(v, bool):
        return float(v)
    return None


def _component_extra(snap: dict, kind: str, key: str) -> float | None:
    """Read a numeric extra from the first `kind` component of a snapshot."""
    for comp in (snap.get("components") or []):
        if comp.get("kind") == kind:
            return _num((comp.get("extras") or {}).get(key))
    return None


def _component_extra_str(snap: dict, kind: str, key: str) -> str | None:
    """Read a non-empty string extra from the first `kind` component."""
    for comp in (snap.get("components") or []):
        if comp.get("kind") == kind:
            v = (comp.get("extras") or {}).get(key)
            return v if isinstance(v, str) and v else None
    return None


# Realtime `ttfb_anchor_source` values the runner writes on the wire.
# `turn_start` means "no anchor": the
# turn's `ttfb_ms` is measured from turn creation (≈ 0) and must never be read
# as latency or E2E. Legacy captures carry no source key; when they still
# carry `user_speech_end_epoch_ms` the reader labels the anchor `provider`
# (anchored, provenance unrecorded — and `ttfb_ms` may be first-*text*
# stamped on those captures).
TTFB_ANCHOR_TURN_START = "turn_start"
TTFB_ANCHOR_LEGACY_PROVIDER = "provider"

# Per-turn input audio quality keys, mirroring
# `audio::input_meter::EXTRAS_KEYS` in voice-pipes. All numeric except
# `input_level_source`.
INPUT_AUDIO_KEYS = (
    "input_speech_dbfs",
    "input_peak_dbfs",
    "input_noise_dbfs",
    "input_snr_db",
    "input_clipped_ratio",
    "input_speech_ratio",
    "input_speech_ms",
    "input_level_source",
)

# Sanity bound for a wallclock speech-end → first-audio pair: clock skew or
# mis-attribution produce negative or absurd gaps; those fall through to the
# estimate path rather than being reported.
_E2E_MAX_DELTA_MS = 60_000.0


def project_snapshot_metrics(snap: dict | None) -> dict:
    """Project a canonical ``TurnMetricsSnapshot`` into flat canonical metric
    fields — the single source of truth for reading per-turn latencies in the
    BFF. Mirrors the frontend accessors in ``lib/metrics/lookup.ts`` and uses
    the canonical VPO key names.

    Keys returned (all canonical, value or None):
      total_ms, processing_ms, endpointing_ms (STT);
      ttft_ms, generation_ms (LLM);
      ttfb_ms, synthesis_ms, audio_duration_ms (TTS / realtime S2S).

    ``ttfb_ms`` is the runner-observed time-to-first-byte the engine writes
    onto the TTS component (see transport/serve/turn.rs) — the perceived first
    audio byte, not the pre-pacer component-internal value.
    """
    out: dict = {
        "total_ms": None,
        "processing_ms": None,
        "endpointing_ms": None,
        "ttft_ms": None,
        "llm_perceived_ttft_ms": None,
        "llm_wait_ms": None,
        "llm_stage_skipped_reason": None,
        "generation_ms": None,
        "ttfb_ms": None,
        "tts_internal_ttfb_ms": None,
        "tts_wait_ms": None,
        "output_wait_ms": None,
        "commit_to_first_audio_ms": None,
        "tts_stage_skipped_reason": None,
        "synthesis_ms": None,
        "audio_duration_ms": None,
        "rtf": None,
        "rtfx": None,
        "chars": None,
        "tokens_per_sec": None,
        "decode_ms": None,
        "prompt_ingest_ms": None,
        "emit_rtf": None,
        "source_rtf": None,
        # STT real-time factor (audio/wall). >1 → the model transcribes faster
        # than realtime, so processing overlaps the user's speech; the generic
        # critical-path-latency model uses it to bound STT's on-path share.
        "stt_rtfx": None,
        "stt_audio_duration_ms": None,
        # Who opened the turn: "user" (default; absent on old captures) or
        # "agent" (out-of-turn speech — idle prompts, filler follow-ups).
        # Agent turns have no user-utterance anchor: they carry no STT
        # component and must not feed user-perceived latency aggregates.
        "initiator": "user",
        # Realtime S2S anchoring. All None
        # on cascade turns and on captures that predate the keys.
        #   realtime                — True when the snapshot carries a
        #                             `kind: "realtime"` component (else None)
        #   first_text_ms           — anchor → first output-transcript delta
        #                             (diagnostic: "the model started")
        #   ttfb_anchor_source      — wire value, or `provider` on legacy
        #                             anchored captures (no source key but
        #                             `user_speech_end_epoch_ms` present),
        #                             None when unanchored
        #   user_speech_end_epoch_ms — wall clock of the provider anchor
        #   vp_speech_end_epoch_ms  — vp's own uplink-VAD end of speech
        #   provider_endpointing_ms — provider anchor − vp stamp: how late the
        #                             provider noticed the end of speech; the
        #                             S2S `endpointing_ms`
        "realtime": None,
        "first_text_ms": None,
        "ttfb_anchor_source": None,
        "user_speech_end_epoch_ms": None,
        "vp_speech_end_epoch_ms": None,
        "provider_endpointing_ms": None,
        # Input audio quality: the user speech
        # this turn answers, as the server received it, metered by vp's own
        # uplink VAD. S2S turns carry it on the `realtime` component, cascade
        # turns on `stt`. dBFS of the normalised signal; all None on captures
        # that predate the keys or when the observer is disabled.
        **dict.fromkeys(INPUT_AUDIO_KEYS),
    }
    if not isinstance(snap, dict):
        return out
    out["total_ms"] = _num(snap.get("total_ms"))
    if snap.get("initiator") == "agent":
        out["initiator"] = "agent"

    def _comp(kind: str):
        for comp in (snap.get("components") or []):
            if comp.get("kind") == kind:
                return comp
        return None

    stt = _comp("stt")
    if stt is not None:
        ex = stt.get("extras") or {}
        proc = _num(ex.get("processing_ms"))
        out["processing_ms"] = proc if proc is not None else _num(stt.get("latency_ms"))
        out["endpointing_ms"] = _num(ex.get("endpointing_ms"))
        out["stt_rtfx"] = _num(ex.get("stt_rtfx"))
        out["stt_audio_duration_ms"] = _num(ex.get("stt_audio_duration_ms"))
    llm = _comp("llm")
    if llm is not None:
        ex = llm.get("extras") or {}
        out["ttft_ms"] = _num(ex.get("ttft_ms"))
        out["llm_perceived_ttft_ms"] = _num(ex.get("llm_perceived_ttft_ms"))
        out["llm_wait_ms"] = _num(ex.get("llm_wait_ms"))
        llm_skip = ex.get("stage_skipped_reason")
        if isinstance(llm_skip, str) and llm_skip:
            out["llm_stage_skipped_reason"] = llm_skip
        gen = _num(ex.get("generation_ms"))
        out["generation_ms"] = gen if gen is not None else _num(llm.get("latency_ms"))
        out["tokens_per_sec"] = _num(ex.get("tokens_per_sec"))
        out["decode_ms"] = _num(ex.get("decode_ms"))
        out["prompt_ingest_ms"] = _num(ex.get("prompt_ingest_ms"))
    tts = _comp("tts")
    if tts is not None:
        ex = tts.get("extras") or {}
        out["ttfb_ms"] = _num(ex.get("ttfb_ms"))
        out["tts_internal_ttfb_ms"] = _num(ex.get("tts_internal_ttfb_ms"))
        out["tts_wait_ms"] = _num(ex.get("tts_wait_ms"))
        out["output_wait_ms"] = _num(ex.get("output_wait_ms"))
        out["commit_to_first_audio_ms"] = _num(ex.get("commit_to_first_audio_ms"))
        tts_skip = ex.get("stage_skipped_reason")
        if isinstance(tts_skip, str) and tts_skip:
            out["tts_stage_skipped_reason"] = tts_skip
        out["synthesis_ms"] = _num(ex.get("synthesis_ms"))
        out["audio_duration_ms"] = _num(ex.get("audio_duration_ms"))
        out["rtf"] = _num(ex.get("rtf"))
        out["rtfx"] = _num(ex.get("rtfx"))
        out["chars"] = _num(ex.get("chars"))
    output_comp = _comp("output")
    if output_comp is not None:
        ex = output_comp.get("extras") or {}
        if out["output_wait_ms"] is None:
            out["output_wait_ms"] = _num(ex.get("output_wait_ms"))
        if out["commit_to_first_audio_ms"] is None:
            out["commit_to_first_audio_ms"] = _num(ex.get("commit_to_first_audio_ms"))
        out["emit_rtf"] = _num(ex.get("emit_rtf"))
        out["source_rtf"] = _num(ex.get("source_rtf"))
    # Realtime S2S collapses STT+LLM+TTS into one component: first audio byte
    # and audio output duration are the only latencies the provider exposes.
    rt = _comp("realtime")
    if rt is not None:
        ex = rt.get("extras") or {}
        out["realtime"] = True
        if out["ttfb_ms"] is None:
            out["ttfb_ms"] = _num(ex.get("ttfb_ms"))
        if out["audio_duration_ms"] is None:
            out["audio_duration_ms"] = _num(ex.get("audio_output_ms"))
        out["first_text_ms"] = _num(ex.get("first_text_ms"))
        out["user_speech_end_epoch_ms"] = _num(ex.get("user_speech_end_epoch_ms"))
        out["vp_speech_end_epoch_ms"] = _num(ex.get("vp_speech_end_epoch_ms"))
        out["provider_endpointing_ms"] = _num(ex.get("provider_endpointing_ms"))
        src = ex.get("ttfb_anchor_source")
        if isinstance(src, str) and src:
            out["ttfb_anchor_source"] = src
        elif out["user_speech_end_epoch_ms"] is not None:
            out["ttfb_anchor_source"] = TTFB_ANCHOR_LEGACY_PROVIDER
    for comp in (stt, rt):
        ex = (comp or {}).get("extras") or {}
        if _num(ex.get("input_speech_dbfs")) is None:
            continue
        for key in INPUT_AUDIO_KEYS:
            if key == "input_level_source":
                src = ex.get(key)
                out[key] = src if isinstance(src, str) and src else None
            else:
                out[key] = _num(ex.get(key))
        break
    return out


def _realtime_extra_number(utt: dict, key: str) -> float | None:
    """Read a numeric extra from the `kind: "realtime"` component of an
    utterance's `metric_snapshot`. Mirrors `realtime*` helpers in
    frontend lib/metrics/lookup.ts. Returns None when absent/non-numeric."""
    return _component_extra(utt.get("metric_snapshot") or {}, "realtime", key)


def realtime_ttfb_anchor_source(utt: dict) -> str | None:
    """The realtime turn's `ttfb_anchor_source` as the reader labels it: the
    wire value, `provider` for a legacy anchored capture, None when the turn
    is unanchored or not realtime (see project_snapshot_metrics)."""
    return project_snapshot_metrics(utt.get("metric_snapshot"))["ttfb_anchor_source"]


def stt_speech_end_anchor(
    event_ts_ms: float | None, payload: dict | None
) -> tuple[float | None, str]:
    """When the user stopped speaking, from a `stt_timing` event.

    The event is emitted at the *commit*, and a turn commits only after the
    pipeline has waited out its silence timeout: `endpointing_ms` after the
    speaker actually stopped. Anchoring the perceived wait on the event's own
    timestamp therefore leaves that whole wait out. On a session with a 700 ms
    timeout it reported a barge-in turn's reply as arriving in 333 ms, beside
    1.5 to 1.7 s for every other turn of the same conversation -- the number
    was not comparable to its neighbours, and it flattered exactly the turns
    a reader looks hardest at.

    The event carries `endpointing_ms`, so the end of speech is recoverable:
    the commit instant less the endpointing. That is labelled
    `stt_speech_end`. When the payload has no usable endpointing the commit
    instant is all there is, and it keeps the `stt_timing` label so a reader
    can tell a derived end of speech from a bare commit time.
    """
    if not isinstance(event_ts_ms, (int, float)):
        return None, "stt_timing"
    endpointing = (payload or {}).get("endpointing_ms")
    if (
        isinstance(endpointing, (int, float))
        and not isinstance(endpointing, bool)
        and 0 < endpointing <= 60_000
    ):
        return float(event_ts_ms) - float(endpointing), "stt_speech_end"
    return float(event_ts_ms), "stt_timing"


def pick_client_speech_end(
    speech_ends: list[float], first_audio_ms: float | None
) -> float | None:
    """The client's end of speech for a turn: the LAST one reported before the
    reply's first audio.

    A detector cannot tell a pause from the end of a turn, so a client may
    report several `speech_end`s for one utterance — "…the internal bond in,
    uh, particle boards" produces one at the "uh" and one at the end. Taking
    the first measured a reply from the pause: 7.7 s reported for a turn that
    took 1.1 s. The last one is when the speaker actually stopped.

    Bounded by the first audio because an end that lands after the reply has
    begun belongs to whatever the user said next, not to the turn the reply
    answers; using it would make the wait negative. With no audio on record
    the bound is dropped and the last report stands.
    """
    ends = [t for t in speech_ends if isinstance(t, (int, float))]
    if not ends:
        return None
    if isinstance(first_audio_ms, (int, float)):
        before = [t for t in ends if t <= first_audio_ms]
        # Every report after the audio: nothing here anchors this turn, and
        # the earliest is the least wrong thing to say.
        return max(before) if before else min(ends)
    return max(ends)


def resolve_speech_end_anchor(utt: dict) -> tuple[float | None, str | None]:
    """Best available end-of-user-speech wall clock for one turn, and its
    provenance. Per turn, in order:

      1. `client_speech_end` — the client's own VAD end of speech (a
         `client_timing` `speech_end` report; client wall clock, so it pairs
         with the client's `audio_receive` stamp skew-free);
      2. `vp_vad` — vp's uplink-VAD observer (`vp_speech_end_epoch_ms`);
      3. the provider anchor (`user_speech_end_epoch_ms`), labelled with the
         wire `ttfb_anchor_source` (`provider_speech_stopped`,
         `input_transcript_proxy`, …) or `provider` on legacy captures;
      4. the cascade wallclock fallback already on the utterance
         (`stt_timing` = STT commit; `utterance_received` = text submit /
         legacy transcript arrival).

    A `text_submit` turn is anchored at the submit regardless of any VAD
    stamp (there was no speech to end). A `turn_start` turn (the runner says
    it had NO anchor) only ever resolves to a client stamp: the cascade
    fallback would be the S2S `utterance_received` event, which fires at
    input-transcript *arrival* and is not an end of speech. Sources are
    never mixed within a turn; different turns of one session may
    legitimately resolve to different sources, and each says which.
    """
    wc = utt.get("_wallclock_ts_ms") or {}
    snap = utt.get("metric_snapshot") or {}
    rt_source = _component_extra_str(snap, "realtime", "ttfb_anchor_source")
    provider_end = _component_extra(snap, "realtime", "user_speech_end_epoch_ms")
    if rt_source == "text_submit" and provider_end is not None:
        return provider_end, "text_submit"
    client_end = wc.get("client_speech_end_ms")
    if isinstance(client_end, (int, float)):
        return float(client_end), "client_speech_end"
    vp_end = _component_extra(snap, "realtime", "vp_speech_end_epoch_ms")
    if vp_end is not None:
        return vp_end, "vp_vad"
    if provider_end is not None:
        return provider_end, rt_source or TTFB_ANCHOR_LEGACY_PROVIDER
    if rt_source == TTFB_ANCHOR_TURN_START:
        return None, None
    fallback = wc.get("user_audio_end_ms")
    if isinstance(fallback, (int, float)):
        return float(fallback), wc.get("user_audio_end_source") or "stt_timing"
    return None, None


def estimate_realtime_e2e(utt: dict) -> tuple[float, str] | None:
    """Server-side E2E estimate for a realtime turn whose client never sent
    an `audio_receive` report (headless / bench / legacy clients).

    The anchored `ttfb_ms` (anchor → first audio byte at the server) is the
    perceived E2E minus network delivery. With a vp VAD stamp the estimate
    is re-anchored there (`provider_endpointing_ms + ttfb_ms`, source
    `vp_vad`) so the provider's endpointing lag is included. Returns
    `(ms, anchor_source)` or None — always None when the turn is unanchored
    (`turn_start`, or a legacy capture without `user_speech_end_epoch_ms`),
    because an unanchored ttfb is ≈ 0 and must never masquerade as E2E.
    """
    snap = utt.get("metric_snapshot") or {}
    source = _component_extra_str(snap, "realtime", "ttfb_anchor_source")
    if source == TTFB_ANCHOR_TURN_START:
        return None
    if _component_extra(snap, "realtime", "user_speech_end_epoch_ms") is None:
        return None
    ttfb = _component_extra(snap, "realtime", "ttfb_ms")
    if ttfb is None:
        return None
    endpt = _component_extra(snap, "realtime", "provider_endpointing_ms")
    if endpt is not None and _component_extra(snap, "realtime", "vp_speech_end_epoch_ms") is not None:
        return float(ttfb + endpt), "vp_vad"
    return float(ttfb), source or TTFB_ANCHOR_LEGACY_PROVIDER


def _int_or_none(v) -> int | None:
    f = _num(v)
    return int(f) if f is not None else None


def _component_name(comp: dict, prefix: str) -> str:
    """`audio_post:<name>` / `annotator:<name>` → `<name>` (falls back to
    `provider`, then the bare component_id)."""
    cid = str(comp.get("component_id") or "")
    if cid.startswith(prefix + ":") and len(cid) > len(prefix) + 1:
        return cid[len(prefix) + 1:]
    return str(comp.get("provider") or cid or prefix)


def project_throughput_metrics(snap: dict | None) -> dict:
    """Project the throughput components of a ``TurnMetricsSnapshot``
    into flat per-turn blocks.

    Returns::

        {
          "output":     {emit_rtf, source_rtf, chain_overhead_ms, pacer_wait_ms,
                         audio_output_ms} | None,
          "audio_post": {name: {calls, latency_p50_ms, latency_max_ms, timeouts,
                                failures, budget_ratio}},        # {} when none
          "annotators": {name: {segments, annotations, latency_p50_ms, latency_max_ms,
                                lag_max_ms, dropped_chunks, timeouts, skipped_segments,
                                breaker_open}},                  # {} when none
        }

    ``output`` is absent (None) on turns with no audio and on captures that
    predate the component. rtf values follow vp's convention: wall ÷ audio,
    > 1.0 = slower than real time. Every field is optional on the wire (e.g.
    ``latency_p50_ms`` / ``budget_ratio`` are omitted when a processor made no
    calls) and comes back as None here.
    """
    out: dict = {"output": None, "audio_post": {}, "annotators": {}}
    if not isinstance(snap, dict):
        return out
    post_latency_total: dict[str, float | None] = {}
    for comp in (snap.get("components") or []):
        if not isinstance(comp, dict):
            continue
        kind = comp.get("kind")
        ex = comp.get("extras") or {}
        if kind == "output" and out["output"] is None:
            out["output"] = {
                "emit_rtf": _num(ex.get("emit_rtf")),
                "source_rtf": _num(ex.get("source_rtf")),
                "chain_overhead_ms": _int_or_none(ex.get("chain_overhead_ms")),
                "audio_output_ms": _int_or_none(ex.get("audio_output_ms")),
            }
        elif kind == "audio_post":
            name = _component_name(comp, "audio_post")
            out["audio_post"][name] = {
                "calls": _int_or_none(ex.get("calls")),
                "latency_p50_ms": _num(ex.get("latency_p50_ms")),
                "latency_max_ms": _num(ex.get("latency_max_ms")),
                "timeouts": _int_or_none(ex.get("timeouts")),
                "failures": _int_or_none(ex.get("failures")),
                "budget_ratio": _num(ex.get("budget_ratio")),
            }
            post_latency_total[name] = _num(comp.get("latency_ms"))
        elif kind == "annotator":
            name = _component_name(comp, "annotator")
            out["annotators"][name] = {
                "segments": _int_or_none(ex.get("segments")),
                "annotations": _int_or_none(ex.get("annotations")),
                "latency_p50_ms": _num(ex.get("latency_p50_ms")),
                "latency_max_ms": _num(ex.get("latency_max_ms")),
                "lag_max_ms": _num(ex.get("lag_max_ms")),
                "dropped_chunks": _int_or_none(ex.get("dropped_chunks")),
                "timeouts": _int_or_none(ex.get("timeouts")),
                "skipped_segments": _int_or_none(ex.get("skipped_segments")),
                "breaker_open": bool(ex.get("breaker_open")) if ex.get("breaker_open") is not None else None,
            }
    if out["output"] is not None:
        _split_chain_overhead(out["output"], post_latency_total)
    return out


def _split_chain_overhead(output: dict, post_latency_total: dict[str, float | None]) -> None:
    """Split the engine's ``chain_overhead_ms`` into the post-processors'
    share and the rest.

    The engine reports ``emit_wall_ms − source_wall_ms``: everything between
    receiving the audio and handing it to the transport. That includes the
    inline post-processors and also the realtime pacer, which holds audio
    back to playback speed (a provider that delivers at 0.23× realtime is
    emitted at 1.0×, and the difference is pacer wait, not processing).

    * No ``audio_post`` component: no processor ran, so ``chain_overhead_ms``
      is 0 and the whole difference is ``pacer_wait_ms``.
    * Processors with their summed call time (the component's
      ``latency_ms``): ``chain_overhead_ms`` is that sum, capped at the
      difference, and ``pacer_wait_ms`` the remainder.
    * Processors without a call-time total: the difference cannot be
      apportioned, so it stays in ``chain_overhead_ms`` and
      ``pacer_wait_ms`` is None.
    """
    raw = output.get("chain_overhead_ms")
    output["pacer_wait_ms"] = None
    if raw is None:
        return
    if not post_latency_total:
        output["chain_overhead_ms"] = 0
        output["pacer_wait_ms"] = raw
        return
    totals = list(post_latency_total.values())
    if any(t is None for t in totals):
        return
    chain = min(raw, round(sum(totals)))
    output["chain_overhead_ms"] = chain
    output["pacer_wait_ms"] = raw - chain


def arrival_rtf(playback_health: dict | None) -> float | None:
    """Client-side arrival realtime factor: wall time first → last audio frame
    received ÷ audio duration received (``arrival_wall_ms / audio_ms``). > 1.0
    means the server delivered audio slower than the client plays it. None when
    either field is missing (old captures) or ``audio_ms`` is 0."""
    if not isinstance(playback_health, dict):
        return None
    wall = _num(playback_health.get("arrival_wall_ms"))
    audio = _num(playback_health.get("audio_ms"))
    if wall is None or audio is None or audio <= 0:
        return None
    return round(wall / audio, 2)


def is_empty_turn(u: dict) -> bool:
    """True when an utterance produced no signal at all — no metrics snapshot,
    no transcript, and no interrupt/terminate flag. These are capture artifacts
    (e.g. the session ended just as a turn began) and MUST NOT count toward turn
    totals or the verdict's "across N turns"."""
    s = u.get("summary") or {}
    if s.get("interrupted") or s.get("terminated"):
        return False
    if u.get("metric_snapshot"):
        return False
    return not (u.get("stt") or u.get("user_text") or u.get("llm_output") or u.get("retained_text"))


def _compute_session_metrics(utterances: list[dict], all_events: list[dict]) -> dict:
    """Backend mirror of frontend `computeSessionMetrics` (metrics.ts).

    Computed over the FULL event log on disk (not paginated) so the right-hand
    Telemetry panel is accurate even for sessions with >200 events.
    """
    def _avg(vals: list[float]) -> float | None:
        return (sum(vals) / len(vals)) if vals else None

    def _round(v: float | None) -> int | None:
        return round(v) if v is not None else None

    # Drop empty/phantom turns so they don't inflate completed/total counts.
    utterances = [u for u in utterances if not is_empty_turn(u)]

    valid = [u for u in utterances if not (u.get("summary") or {}).get("terminated")]

    def _summary_field(u: dict, key: str):
        s = u.get("summary") or {}
        return s.get(key)

    # Latency metrics come from the canonical per-turn snapshot (single source
    # of truth); lifecycle/barge-in fields stay on the summary (injected from
    # pipeline events). See project_snapshot_metrics.
    proj = {id(u): project_snapshot_metrics(u.get("metric_snapshot")) for u in valid}
    ttft_vals = [v for v in (proj[id(u)]["ttft_ms"] for u in valid) if isinstance(v, (int, float))]
    ttfb_vals = [v for v in (proj[id(u)]["ttfb_ms"] for u in valid) if isinstance(v, (int, float))]
    endpt_vals = [v for v in (proj[id(u)]["endpointing_ms"] for u in valid) if isinstance(v, (int, float))]
    # S2S provider endpointing: kept apart
    # from the cascade `endpointing_ms` average — different mechanism
    # (provider VAD/transcription lag vs vp's own VAD silence wait).
    provider_endpt_vals = [
        v for v in (proj[id(u)]["provider_endpointing_ms"] for u in valid)
        if isinstance(v, (int, float))
    ]
    rtt_vals = [v for v in (_summary_field(u, "barge_in_rtt_ms") for u in valid) if isinstance(v, (int, float))]

    def _interrupted(u: dict, trigger: str) -> bool:
        s = u.get("summary") or {}
        return bool(s.get("interrupted")) and s.get("interrupt_trigger") == trigger

    server_barge_ins = sum(1 for u in valid if _interrupted(u, "server_vad"))
    client_barge_ins = sum(1 for u in valid if _interrupted(u, "client_cancel"))
    playback_barge_ins = sum(
        1 for u in valid
        if not (u.get("summary") or {}).get("interrupted") and (u.get("summary") or {}).get("client_interrupted")
    )

    # Tokens — prefer llm_detail, fall back to llm_timing if no llm_detail exists.
    llm_details = [e for e in all_events if (e.get("event_type") or e.get("type")) == "llm_detail"]
    llm_timings_fb = (
        [e for e in all_events if (e.get("event_type") or e.get("type")) == "llm_timing"]
        if not llm_details else []
    )
    norm_llm = [_normalize_llm_payload(e.get("payload") or {}) for e in (llm_details + llm_timings_fb)]
    total_prompt_tokens = sum((t["prompt_tokens"] or 0) for t in norm_llm)
    total_output_tokens = sum((t["output_tokens"] or 0) for t in norm_llm)
    tok_sec_vals = [t["tokens_per_sec"] for t in norm_llm if isinstance(t["tokens_per_sec"], (int, float))]

    # TTS — derive audio duration and RTF from canonical turn snapshots;
    # fall back to tts_detail (preferred) or tts_timing (if no tts_detail), never summing both.
    snap_audio_ms = [proj[id(u)]["audio_duration_ms"] for u in valid if isinstance(proj[id(u)]["audio_duration_ms"], (int, float))]
    snap_rtf = [proj[id(u)]["rtf"] for u in valid if isinstance(proj[id(u)]["rtf"], (int, float))]

    if snap_audio_ms:
        total_audio_seconds = sum(snap_audio_ms) / 1000.0
        rtf_vals = snap_rtf
    else:
        tts_details = [e for e in all_events if (e.get("event_type") or e.get("type")) == "tts_detail"]
        tts_events = tts_details if tts_details else [
            e for e in all_events
            if (e.get("event_type") or e.get("type")) == "tts_timing"
        ]
        total_audio_seconds = sum(((e.get("payload") or {}).get("audio_duration_s") or 0) for e in tts_events)
        rtf_vals = [v for v in ((e.get("payload") or {}).get("rtf") for e in tts_events) if isinstance(v, (int, float))]

    # Realtime S2S fallback: provider-native sessions (Gemini Live / OpenAI
    # Realtime) emit no `tts_detail`/`tts_timing` events. Their per-turn audio
    # duration lives in the `kind: "realtime"` component of `metric_snapshot`
    # under the `audio_output_ms` extra. Add it so the Telemetry panel's
    # "Total Audio" is non-zero for realtime sessions. See lib/metrics/lookup.ts.
    if total_audio_seconds == 0:
        for u in valid:
            ms = _realtime_extra_number(u, "audio_output_ms")
            if ms is not None:
                total_audio_seconds += ms / 1000.0

    terminated = sum(1 for u in utterances if (u.get("summary") or {}).get("terminated"))

    avg_tok_sec = _avg(tok_sec_vals)
    avg_rtf = _avg(rtf_vals)

    # Streaming-phase output throughput:
    # `emit_rtf` from the `output` component, over EVERY non-empty turn —
    # interrupted and terminated turns included, since a turn that starves
    # the client is usually the one that gets cut short. `avgRtf` keeps its
    # cascade-TTS synthesis meaning; the report populates `avg_rtf` from
    # `emit_rtf` for s2s pipelines (render._build_report_for_llm).
    emit_rtf_vals = [
        v for v in (
            (project_throughput_metrics(u.get("metric_snapshot"))["output"] or {}).get("emit_rtf")
            for u in utterances
        )
        if isinstance(v, (int, float))
    ]
    avg_emit_rtf = _avg(emit_rtf_vals)

    return {
        "completedTurns": len(utterances) - terminated,
        "terminatedTurns": terminated,
        "totalTurns": len(utterances),
        "avgTtftMs": _round(_avg(ttft_vals)),
        "avgTtfbMs": _round(_avg(ttfb_vals)),
        "avgEndpointingMs": _round(_avg(endpt_vals)),
        "avgProviderEndpointingMs": _round(_avg(provider_endpt_vals)),
        "avgBargeInRttMs": _round(_avg(rtt_vals)),
        "serverBargeIns": server_barge_ins,
        "clientBargeIns": client_barge_ins,
        "playbackBargeIns": playback_barge_ins,
        "totalPromptTokens": total_prompt_tokens,
        "totalOutputTokens": total_output_tokens,
        "avgTokensPerSec": (round(avg_tok_sec * 10) / 10) if avg_tok_sec is not None else None,
        "totalAudioSeconds": round(total_audio_seconds * 100) / 100,
        "avgRtf": (round(avg_rtf * 100) / 100) if avg_rtf is not None else None,
        # Output throughput rollup. None /
        # 0 on captures that predate the `output` component.
        "avgEmitRtf": (round(avg_emit_rtf * 100) / 100) if avg_emit_rtf is not None else None,
        "maxEmitRtf": (round(max(emit_rtf_vals) * 100) / 100) if emit_rtf_vals else None,
        "turnsOverRealtime": sum(1 for v in emit_rtf_vals if v > 1.0),
        # Playback-health rollup: client-reported underruns/silence
        # aggregated over the session.
        "turnsWithUnderruns": sum(
            1 for u in utterances
            if ((u.get("playback_health") or {}).get("underrun_count") or 0) > 0
        ),
        "totalUnderruns": sum(
            (u.get("playback_health") or {}).get("underrun_count") or 0 for u in utterances
        ),
        "totalSilenceMs": sum(
            (u.get("playback_health") or {}).get("total_silence_ms") or 0 for u in utterances
        ),
        # Provenance: how many events were aggregated. Lets the UI show "Computed
        # from full log (N events)" instead of the partial-data banner.
        "eventsAggregated": len(all_events),
    }

def _metrics_richness(m: dict | None) -> tuple[int, float]:
    """Rank a ``turn_metrics_ready`` snapshot by how much data it carries.

    An interrupted turn emits a *complete* snapshot when the response finishes,
    then a *reset* snapshot (``total_ms=0``, ``components=[]``) at barge-in.
    Last-write-wins would let that empty reset clobber the real per-component
    metrics (TTFB / synthesis_ms / total_ms), leaving an interrupted turn with
    no Response/TTFB even though the response actually completed before the
    user barged in. We keep whichever snapshot ranks higher: more components
    first, then larger ``total_ms``.
    """
    if not m:
        return (-1, -1.0)
    comps = m.get("components") or []
    return (len(comps), float(m.get("total_ms") or 0.0))


def _collect_client_tool_calls(all_events: list[dict], evt_ts_ms) -> list[dict]:
    """Pair client-tool trace events into a per-call activity list.

    Input events (component ``client_tools``, written by the worker):
      - ``client_tool_call``:   { call_id, name, arguments, speak_on_result }
      - ``client_tool_result``: { call_id, name, phase, result, spoken }

    Output rows (insertion-ordered by first sighting):
      { call_id, name, arguments, speak_on_result, called_at_ms,
        status: pending|completed|failed, result, spoken, resolved_at_ms,
        acked_at_ms? (first started/progress phase, when the client sent one) }

    The worker only records *accepted* resolutions (duplicates and
    unknown ids are dropped), so the first terminal result per call is
    authoritative; later ones are ignored defensively anyway.
    """
    calls: dict[str, dict] = {}
    order: list[str] = []

    def _entry(call_id: str) -> dict:
        if call_id not in calls:
            calls[call_id] = {"call_id": call_id, "status": "pending"}
            order.append(call_id)
        return calls[call_id]

    for evt in all_events:
        etype = evt.get("event_type") or evt.get("type")
        if etype not in ("client_tool_call", "client_tool_result"):
            continue
        p = evt.get("payload") or {}
        call_id = p.get("call_id")
        if not call_id:
            continue
        entry = _entry(str(call_id))
        if etype == "client_tool_call":
            entry["name"] = p.get("name")
            entry["arguments"] = p.get("arguments")
            entry["speak_on_result"] = p.get("speak_on_result")
            entry["called_at_ms"] = evt_ts_ms(evt)
            # The turn the worker dispatched the call from, when it says.
            if isinstance(p.get("utterance_id"), int):
                entry["utterance_id"] = p["utterance_id"]
        else:
            if entry["status"] in ("completed", "failed"):
                continue
            phase = p.get("phase")
            if phase in ("started", "progress"):
                # Interim ack: the client acknowledged the call and is
                # executing. First sighting wins — `acked_at_ms` is the
                # responsiveness signal that separates a long-running
                # action (by design) from an unresponsive integration.
                if "acked_at_ms" not in entry:
                    entry["acked_at_ms"] = evt_ts_ms(evt)
                continue
            if phase in ("completed", "failed"):
                entry["status"] = phase
                entry["result"] = p.get("result")
                entry["spoken"] = p.get("spoken")
                # The client's own opt-in flag, distinct from `spoken`
                # (the final decision, which the tool's speak_on_result
                # policy can force). Lets the UI attribute speech to
                # "tool policy" vs "client request".
                entry["speak_requested"] = p.get("speak_requested")
                entry["resolved_at_ms"] = evt_ts_ms(evt)
                if entry.get("name") is None:
                    entry["name"] = p.get("name")

    return [calls[c] for c in order]


def _attribute_calls_to_turns(calls: list[dict], utterances: list[dict]) -> None:
    """Stamp each client tool call with ``after_utterance_id``: the last
    turn whose *output started* before the call was dispatched (None =
    before the first turn). The anchor prefers the client's
    ``first_audio_out_ms`` (audio_receive wallclock) and falls back to
    ``user_audio_end_ms`` — a tool call fired between turn N's output and
    turn N+1's output renders after turn N's card. Turns without any
    wallclock (headless clients, legacy captures) are skipped; calls that
    can't be placed keep ``after_utterance_id = None`` and render at the
    top of the turns list.
    """
    anchors: list[tuple[int, float]] = []
    for u in utterances:
        uid = u.get("utterance_id")
        wc = u.get("_wallclock_ts_ms") or {}
        ts = wc.get("first_audio_out_ms") or wc.get("user_audio_end_ms")
        if isinstance(uid, int) and isinstance(ts, (int, float)):
            anchors.append((uid, float(ts)))
    for call in calls:
        # The worker names the owning turn for calls it dispatched from one.
        # That is exact where the timestamps below cannot be: a turn that
        # only called a tool produced no audio, so it has no anchor of its
        # own and its call would land on the turn before.
        if isinstance(call.get("utterance_id"), int):
            call["after_utterance_id"] = call["utterance_id"]
            continue
        t = call.get("called_at_ms")
        after = None
        if isinstance(t, (int, float)):
            for uid, ts in anchors:
                if ts <= t:
                    after = uid
                else:
                    break
        call["after_utterance_id"] = after


def _read_capture_artifacts(
    session_id: str,
    events_offset: int = 0,
    events_limit: int = 200,
    tenant_id: str | None = None,
):
    """Read events.jsonl, telemetry.jsonl, and per-utterance files from captures dir."""
    session_dir = (
        _resolve_session_dir(session_id, tenant_id=tenant_id)
        if isinstance(tenant_id, str)
        else _resolve_session_dir(session_id)
    )
    result = {"events": [], "events_total": 0, "utterances": [], "telemetry": [], "session_metrics": None}

    if not session_dir.exists() or not session_dir.is_dir():
        result["captures_missing"] = True
        return result
    
    result["captures_missing"] = False
    # Torn records are skipped by every reader below; say how many, so a
    # session whose numbers rest on an incomplete event file does not pass
    # for a complete one.
    result["unreadable_event_lines"] = sum(
        count_unreadable_lines(session_dir / name)
        for name in ("pipeline_events.jsonl", "events.jsonl")
    )

    # events.jsonl and pipeline_events.jsonl (merged and paginated)
    events_path = session_dir / "events.jsonl"
    all_events = []
    
    def _evt_ts(evt: dict) -> float:
        ts = evt.get("timestamp") or {}
        if isinstance(ts, dict):
            secs = ts.get("secs_since_epoch")
            nanos = ts.get("nanos_since_epoch") or 0
            if isinstance(secs, (int, float)):
                return float(secs) + float(nanos) / 1e9
        if isinstance(ts, (int, float)):
            return float(ts)
        # Pipeline client timing rows carry no envelope timestamp, only the
        # client's own clock; without it they would all sort to the start.
        client_ms = evt.get("client_timestamp_ms")
        if isinstance(client_ms, (int, float)):
            return float(client_ms) / 1000.0
        return 0.0

    if events_path.exists():
        try:
            with open(events_path, encoding="utf-8") as f:
                all_events = [row for row in (parse_jsonl_line(line) for line in f if line.strip()) if row is not None]
        except (OSError, ValueError):
            # Unreadable file: the session is shown without its events.
            pass

    pipeline_events_path = session_dir / "pipeline_events.jsonl"
    if pipeline_events_path.exists():
        try:
            with open(pipeline_events_path, encoding="utf-8") as f:
                for line in f:
                    if not line.strip():
                        continue
                    evt = parse_jsonl_line(line)
                    if evt is None:
                        continue
                    # Map pipeline `kind` to `type` for UI EventCardRenderer
                    if "kind" in evt and "type" not in evt:
                        evt["type"] = evt["kind"]
                    evt.setdefault("component", "pipeline")
                    all_events.append(evt)
        except (OSError, ValueError):
            # Unreadable file: the session is shown without pipeline events.
            pass

    all_events.sort(key=_evt_ts)
    result["events_total"] = len(all_events)
    result["events"] = all_events[events_offset : events_offset + events_limit]

    # telemetry.jsonl
    telemetry_path = session_dir / "telemetry.jsonl"
    if telemetry_path.exists():
        try:
            with open(telemetry_path, encoding="utf-8") as f:
                result["telemetry"] = [row for row in (parse_jsonl_line(line) for line in f if line.strip()) if row is not None]
        except (OSError, ValueError):
            # Unreadable file: the session is shown without telemetry.
            pass

    # Per-utterance files
    utterances: dict[int, dict] = {}
    for fp in sorted(session_dir.iterdir()):
        m = _UTT_FILE_RE.match(fp.name)
        if not m:
            d = _DROPPED_FILE_RE.match(fp.name)
            if d:
                utt = utterances.setdefault(int(d.group("utt")), {"utterance_id": int(d.group("utt"))})
                utt.setdefault("dropped_recordings", []).append({
                    "file": fp.name,
                    "reason": d.group("reason"),
                    "attempt": int(d.group("attempt") or 1),
                })
            continue
        utt_id = int(m.group("utt"))
        kind = m.group("kind")
        ext = m.group("ext")
        utt = utterances.setdefault(utt_id, {"utterance_id": utt_id})
        try:
            if ext == "json":
                with open(fp, encoding="utf-8") as fh:
                    if kind == "turn_resolved":
                        key = "summary"
                    elif kind == "playback":
                        # NNN_playback.json — client playback health.
                        key = "playback_health"
                    else:
                        key = kind
                    utt[key] = json.load(fh)
            elif ext == "txt":
                with open(fp, encoding="utf-8") as fh:
                    utt[kind] = fh.read()
            elif ext == "wav":
                utt.setdefault("audio_files", {})[kind] = fp.name
        except (OSError, ValueError):
            # One unreadable turn file (cut off mid-write) leaves that
            # artifact out; the rest of the turn is still shown.
            pass
    if utterances:
        result["utterances"] = [utterances[k] for k in sorted(utterances)]

    # Merge barge_in_rtt data from events.jsonl into per-utterance summaries.
    # events.jsonl already read above into all_events.
    for evt in all_events:
        if evt.get("event_type") == "barge_in_rtt":
            p = evt.get("payload", {})
            uid = p.get("utterance_id")
            if uid is not None and uid in utterances:
                utt = utterances[uid]
                summary = utt.get("summary")
                if summary is None:
                    summary = {}
                    utt["summary"] = summary
                summary["barge_in_rtt_ms"] = p.get("rtt_ms")
                summary["last_played_offset_ms"] = p.get("last_played_offset_ms")

    def _evt_ts_ms(evt: dict) -> float | None:
        ts = evt.get("timestamp") or {}
        if isinstance(ts, dict):
            secs = ts.get("secs_since_epoch")
            nanos = ts.get("nanos_since_epoch") or 0
            if isinstance(secs, (int, float)):
                return float(secs) * 1000.0 + float(nanos) / 1_000_000.0
        if isinstance(ts, (int, float)):
            return float(ts)
        return None

    # pipeline_events.jsonl: inject user_text for text-mode turns and
    # retained_text for interrupted turns (HistoryModified events).
    # (text turns have no stt.json, so the user's typed message is only here)
    pipeline_events_path = session_dir / "pipeline_events.jsonl"
    if pipeline_events_path.exists():
        try:
            with open(pipeline_events_path, encoding="utf-8") as f:
                for line in f:
                    if not line.strip():
                        continue
                    evt = parse_jsonl_line(line)
                    if evt is None:
                        continue
                    kind = evt.get("kind")
                    if kind == "utterance_received":
                        utt_id = evt.get("utterance_id")
                        if utt_id is not None:
                            utt = utterances.setdefault(utt_id, {"utterance_id": utt_id})
                            if evt.get("source") == "Text":
                                utt["user_text"] = evt.get("text", "")
                            # Fallback "end of user input" wallclock. Exact for
                            # Text turns (emitted at submit). For Realtime S2S
                            # audio turns it fires at input-transcript *arrival*
                            # — often after response audio already started — so
                            # it is only a last resort for legacy captures; the
                            # authoritative anchors (`stt_timing` for pipeline,
                            # the snapshot's `user_speech_end_epoch_ms` for
                            # S2S) overwrite it below.
                            wc = utt.setdefault("_wallclock_ts_ms", {})
                            if "user_audio_end_ms" not in wc:
                                ts_ms = _evt_ts_ms(evt)
                                if ts_ms is not None:
                                    wc["user_audio_end_ms"] = ts_ms
                                    wc["user_audio_end_source"] = "utterance_received"
                            result["utterances"] = [utterances[k] for k in sorted(utterances)]
                    elif kind == "utterance_dropped":
                        # Speech that came to nothing. The id is the turn the
                        # speech would have become, which the next committed
                        # utterance then takes; so the record hangs off that
                        # id and the turn row reports it as the wait before it.
                        utt_id = evt.get("utterance_id")
                        if utt_id is not None:
                            utt = utterances.setdefault(utt_id, {"utterance_id": utt_id})
                            utt.setdefault("dropped_before", []).append({
                                "reason": evt.get("reason"),
                                "text": evt.get("text") or "",
                                "had_interim_text": bool(evt.get("had_interim_text")),
                                "audio_ms": evt.get("audio_ms"),
                                "at_ms": _evt_ts_ms(evt),
                            })
                            result["utterances"] = [utterances[k] for k in sorted(utterances)]
                    elif kind == "history_modified":
                        mod = evt.get("modification", {})
                        utt_id = mod.get("utterance_id")
                        retained = mod.get("retained_text")
                        if utt_id is not None and retained:
                            utt = utterances.setdefault(utt_id, {"utterance_id": utt_id})
                            utt["retained_text"] = retained
                            result["utterances"] = [utterances[k] for k in sorted(utterances)]
                        # For PlaybackReport-triggered modifications, capture client-side
                        # timing so the UI can show "Playback barge-in" and the ✂ offset.
                        if mod.get("method") == "playback_report" and utt_id is not None:
                            utt = utterances.setdefault(utt_id, {"utterance_id": utt_id})
                            summary = utt.setdefault("summary", {})
                            # Only flag as client_interrupted when the server did NOT
                            # already mark this turn as interrupted (server VAD / cancel).
                            if not summary.get("interrupted"):
                                summary["client_interrupted"] = True
                            # Preserve last_played_offset_ms from barge_in_rtt if set;
                            # otherwise populate from the PlaybackReport offset.
                            if summary.get("last_played_offset_ms") is None:
                                played_ms = mod.get("played_ms")
                                if played_ms is not None:
                                    summary["last_played_offset_ms"] = played_ms
                            result["utterances"] = [utterances[k] for k in sorted(utterances)]
                    elif kind == "interrupted":
                        utt_id = evt.get("utterance_id")
                        trigger = evt.get("trigger")
                        if utt_id is not None and trigger:
                            utt = utterances.setdefault(utt_id, {"utterance_id": utt_id})
                            summary = utt.setdefault("summary", {})
                            summary["interrupt_trigger"] = trigger
                            # pipeline_events.jsonl is the authoritative real-time
                            # record of the interrupt; turn_resolved.json may have been
                            # written before the interrupt was processed (late barge-in
                            # during TTS tail after LLM finished). Force the flag so
                            # the UI always shows the interruption indicator.
                            summary["interrupted"] = True
                            result["utterances"] = [utterances[k] for k in sorted(utterances)]
                    elif kind == "barge_in_resolved":
                        # Authoritative barge-in metadata from the worker
                        # (trigger, RTT, offset-clamped played_ms).
                        #
                        # Deliberately does NOT overwrite retained_text: the
                        # heard text is carried by the `history_modified` event
                        # (word-timed / proportional) for current
                        # captures, and by the proportional fragment-file
                        # fallback below for older ones. Consuming the raw
                        # `barge_in_resolved.retained_text` here would regress
                        # older captures, whose worker recorded the *full*
                        # sentence as "retained" — so we leave text derivation to
                        # those two paths and only adopt the metadata.
                        utt_id = evt.get("utterance_id")
                        if utt_id is not None:
                            utt = utterances.setdefault(utt_id, {"utterance_id": utt_id})
                            summary = utt.setdefault("summary", {})
                            summary["interrupted"] = True
                            if evt.get("trigger"):
                                summary["interrupt_trigger"] = evt["trigger"]
                            if evt.get("barge_in_rtt_ms") is not None:
                                summary["barge_in_rtt_ms"] = evt["barge_in_rtt_ms"]
                            if evt.get("played_ms") is not None:
                                summary["last_played_offset_ms"] = evt["played_ms"]
                            result["utterances"] = [utterances[k] for k in sorted(utterances)]
                    elif kind == "turn_metrics_ready":
                        utt_id = evt.get("utterance_id")
                        metrics = evt.get("metrics")
                        if utt_id is not None and metrics is not None:
                            utt = utterances.setdefault(utt_id, {"utterance_id": utt_id})
                            # Keep the richest snapshot for the turn: an
                            # interrupted turn emits a full snapshot when the
                            # response completes and then an empty reset snapshot
                            # at barge-in — last-write-wins would drop its
                            # TTFB/synthesis/total. See _metrics_richness.
                            if _metrics_richness(metrics) >= _metrics_richness(utt.get("metric_snapshot")):
                                utt["metric_snapshot"] = metrics
                            result["utterances"] = [utterances[k] for k in sorted(utterances)]
        except Exception:
            # A malformed capture must not take the session page down.
            log.warning("pipeline events of session %s were only partly read", session_id, exc_info=True)

    # Per-utterance telemetry normalization.
    #
    # `events.jsonl` carries STT/LLM/TTS timing payloads that mostly do NOT
    # include `utterance_id`. We must match them by sequence using the
    # authoritative lifecycle markers in `pipeline_events.jsonl`.
    #
    # Algorithm:
    #   1. Walk pipeline_events to build expected sequences:
    #        - utterance_received(source=Audio) → expected_stt
    #        - llm_completed                    → expected_llm
    #        - tts_completed                    → expected_tts
    #   2. Walk events.jsonl in order; for each typed event, pop the next
    #      expected utterance_id and attach the payload to that utterance.
    #   3. Interrupted turns naturally drop out — they don't emit the trailing
    #      *_completed marker and so don't claim a slot in the expected sequence.
    expected_stt: list[int] = []
    expected_llm: list[int] = []
    expected_tts: list[int] = []
    if pipeline_events_path.exists():
        try:
            with open(pipeline_events_path, encoding="utf-8") as f:
                for line in f:
                    if not line.strip():
                        continue
                    pe = parse_jsonl_line(line)
                    if pe is None:
                        continue
                    pkind = pe.get("kind")
                    puid = pe.get("utterance_id")
                    if puid is None:
                        continue
                    if pkind == "utterance_received" and pe.get("source") == "Audio":
                        expected_stt.append(puid)
                    elif pkind == "llm_completed":
                        expected_llm.append(puid)
                    elif pkind == "tts_completed":
                        expected_tts.append(puid)
        except (OSError, ValueError):
            # Unreadable file: timing payloads stay unattributed.
            pass

    # Walk events.jsonl in order, assigning each typed payload to its utterance.
    seq_for: dict[str, list[int]] = {
        "stt_timing": expected_stt,
        "llm_timing": expected_llm,
        "llm_detail": expected_llm,
        "tts_timing": expected_tts,
        "tts_detail": expected_tts,
    }
    seq_idx: dict[str, int] = dict.fromkeys(seq_for, 0)
    # Per-utterance wall-clock timestamps for the e2e_perceived rework
    # (same anchor precedence used elsewhere: client VAD, then provider
    # anchor, then turn start):
    #   client_timing.speech_end (last before the reply) = "client VAD: user stopped"
    #   stt_timing event   = "user finished speaking"   (cascade commit proxy)
    #   client_timing.audio_receive (first occurrence)  = "first audio reached client"
    # We attach these to the matching utterance dict under
    # `_wallclock_ts_ms` so `_flatten_turn` can compute a true
    # speech-end → first-audio-out perceived wait.

    for evt in all_events:
        etype = evt.get("event_type") or evt.get("type")
        # Side-channel: capture client_timing audio_receive / speech_end
        # timestamps for the e2e_perceived wall-clock metric.  These events
        # carry utterance_id in their payload, so we don't depend on the
        # lifecycle-marker matcher.
        if etype == "client_timing":
            payload = evt.get("payload") or {}
            evt_name = payload.get("event") or payload.get("timing_event")
            uid_p = payload.get("utterance_id")
            if uid_p is not None and evt_name in ("audio_receive", "speech_end"):
                utt = utterances.setdefault(uid_p, {"utterance_id": uid_p})
                wc = utt.setdefault("_wallclock_ts_ms", {})
                ts = _evt_ts_ms(evt)
                # Prefer the client's own timestamp when present (avoids
                # server clock skew; speech_end → audio_receive is then a
                # client-to-client pair); fall back to the server-side ts.
                cts = payload.get("client_timestamp_ms")
                stamp = float(cts) if isinstance(cts, (int, float)) else ts
                if evt_name == "audio_receive":
                    # First occurrence wins: later chunks are not "first".
                    wc.setdefault("first_audio_out_ms", stamp)
                else:
                    # Every report is kept; which one anchors the turn is
                    # decided once they are all in (see below), because the
                    # answer depends on when the reply's audio began.
                    wc.setdefault("_client_speech_ends", []).append(stamp)

        if etype not in seq_for:
            # Direct-keyed events (client_timing, barge_in_rtt) carry utterance_id
            # in their payload and are handled above; skip here.
            continue
        seq = seq_for[etype]
        i = seq_idx[etype]
        if i >= len(seq):
            # More events of this type than expected (e.g. legacy capture or
            # provider double-emit) — drop the surplus rather than misattribute.
            continue
        uid = seq[i]
        seq_idx[etype] = i + 1
        utt = utterances.setdefault(uid, {"utterance_id": uid})
        utt[etype] = evt.get("payload") or {}
        # The server's own estimate of when the user finished speaking, used
        # when the client sent no `speech_end` -- which is every turn the user
        # started by talking over the agent, since a client report is
        # suppressed while the agent is speaking.
        if etype == "stt_timing":
            wc = utt.setdefault("_wallclock_ts_ms", {})
            anchor, source = stt_speech_end_anchor(_evt_ts_ms(evt), utt[etype])
            wc["user_audio_end_ms"] = anchor
            wc["user_audio_end_source"] = source

    # Settle each turn's client end of speech now that every report and the
    # reply's first audio are both known.
    for utt in utterances.values():
        wc = utt.get("_wallclock_ts_ms")
        if not wc:
            continue
        ends = wc.pop("_client_speech_ends", None)
        if ends:
            chosen = pick_client_speech_end(ends, wc.get("first_audio_out_ms"))
            if chosen is not None:
                wc["client_speech_end_ms"] = chosen
                # How many the client sent, so a report can say the speaker
                # paused rather than leave a reader to infer it.
                wc["client_speech_end_reports"] = len(ends)

    # Synthesize llm_output from TTS fragment text files when llm_output.json was not
    # written (common for turns interrupted before LLM generation completed).
    for utt in utterances.values():
        if "llm_output" not in utt:
            frag_keys = sorted(
                [k for k in utt if _TTS_FRAG_RE.match(k)],
                key=lambda k: int(_TTS_FRAG_RE.match(k).group(1)),  # type: ignore[union-attr]
            )
            if frag_keys:
                parts = [str(utt[k]).strip() for k in frag_keys if str(utt.get(k, "")).strip()]
                if parts:
                    utt["llm_output"] = {"text": " ".join(parts)}
        llm_out = utt.get("llm_output")
        if isinstance(llm_out, dict):
            if not llm_out.get("spoken_text"):
                frag_spoken_keys = sorted(
                    [k for k in utt if _TTS_FRAG_SPOKEN_RE.match(k)],
                    key=lambda k: int(_TTS_FRAG_SPOKEN_RE.match(k).group(1)),  # type: ignore[union-attr]
                )
                if frag_spoken_keys:
                    spoken_parts = [str(utt[k]).strip() for k in frag_spoken_keys if str(utt.get(k, "")).strip()]
                    if spoken_parts:
                        llm_out["spoken_text"] = " ".join(spoken_parts)
                        utt["spoken_text"] = llm_out["spoken_text"]
            else:
                utt["spoken_text"] = llm_out["spoken_text"]

    # Fallback: derive retained_text from TTS fragment files for interrupted turns
    # that don't have a history_modified pipeline event (AudioLedger not populated).
    for utt in utterances.values():
        if utt.get("retained_text") is not None:
            # Already set authoritatively from history_modified / barge_in_resolved
            # (more accurate than the fragment-file heuristic). An explicit empty
            # string means "nothing heard" and must be preserved, so check for
            # presence rather than truthiness.
            continue
        summary = utt.get("summary") or {}
        if not summary.get("interrupted"):
            continue
        offset_ms: float | None = summary.get("last_played_offset_ms")
        if offset_ms is None:
            continue
        # Collect TTS fragment keys in ascending order
        frag_keys = sorted(
            [k for k in utt if _TTS_FRAG_RE.match(k)],
            key=lambda k: int(_TTS_FRAG_RE.match(k).group(1)),  # type: ignore[union-attr]
        )
        if not frag_keys:
            continue
        audio_files = utt.get("audio_files", {})
        accumulated_ms = 0.0
        retained_parts: list[str] = []
        for fk in frag_keys:
            text = str(utt.get(fk, "")).strip()
            wav_name = audio_files.get(fk)
            dur_ms = _wav_duration_ms(session_dir / wav_name) if wav_name else 0.0
            if accumulated_ms + dur_ms <= offset_ms:
                # Fragment fully played
                if text:
                    retained_parts.append(text)
                accumulated_ms += dur_ms
            else:
                # Fragment partially played — proportional word slice
                if dur_ms > 0 and text:
                    frag_fraction = max(0.0, (offset_ms - accumulated_ms) / dur_ms)
                    words = text.split()
                    kept = max(1, round(frag_fraction * len(words)))
                    retained_parts.append(" ".join(words[:kept]))
                break
        if retained_parts:
            utt["retained_text"] = " ".join(retained_parts)

    # Project canonical latency fields from the metric snapshot onto each
    # utterance's `summary` so the session-detail API exposes one canonical
    # vocabulary (total_ms, processing_ms, endpointing_ms, ttft_ms,
    # generation_ms, ttfb_ms, synthesis_ms, audio_duration_ms). The snapshot
    # remains the single source of truth; this is a derived view for the UI.
    # Lifecycle/barge-in fields already on the summary are left untouched.
    for utt in utterances.values():
        snap = utt.get("metric_snapshot")
        if not snap:
            continue
        summary = utt.setdefault("summary", {})
        for k, v in project_snapshot_metrics(snap).items():
            if v is not None and summary.get(k) is None:
                summary[k] = v

    # Project client playback health onto the summary vocabulary.
    # `audio_underruns` feeds the EXISTING TurnCard badge; setdefault keeps
    # any server-derived values.
    for utt in utterances.values():
        ph = utt.get("playback_health")
        if ph:
            summary = utt.setdefault("summary", {})
            summary.setdefault("audio_underruns", ph.get("underrun_count"))
            summary.setdefault("total_silence_ms", ph.get("total_silence_ms"))
            summary.setdefault("wall_clock_ms", ph.get("wall_ms"))
            summary.setdefault("audio_duration_ms", ph.get("audio_ms"))
            summary["playback_gaps"] = ph.get("gaps", [])
            # Derived: arrival wall ÷ audio.
            # None on frames that predate `arrival_wall_ms`.
            rtf = arrival_rtf(ph)
            if rtf is not None:
                summary.setdefault("arrival_rtf", rtf)

    # Per-fragment timing for the session-detail fragment timeline. The API
    # used to expose fragment *filenames* only; the timeline needs where each
    # one sits in the ideal audio. Offsets are cumulative durations in
    # fragment order — exactly how `tts_concat.wav` is assembled — so a seek
    # to `offset_ms` on that track lands on the fragment's first sample.
    # `_wav_duration_ms` reads only the header, so a 400-fragment realtime
    # turn costs 400 small opens, not 400 decodes.
    for utt in utterances.values():
        utt_audio_files = utt.get("audio_files") or {}
        frag_keys = sorted(
            [k for k in utt_audio_files if _TTS_FRAG_RE.match(k)],
            key=lambda k: int(_TTS_FRAG_RE.match(k).group(1)),  # type: ignore[union-attr]
        )
        if not frag_keys:
            continue
        offset_ms = 0.0
        fragments: list[dict] = []
        for fk in frag_keys:
            dur_ms = _wav_duration_ms(session_dir / utt_audio_files[fk])
            fragments.append(
                {"kind": fk, "offset_ms": round(offset_ms, 1), "duration_ms": round(dur_ms, 1)}
            )
            offset_ms += dur_ms
        utt["fragments"] = fragments

    # Where the user actually spoke inside the mic capture. S2S sessions keep
    # the mic open, so `mic_raw` spans the whole inter-turn window (agent
    # speech included) and is mostly silence; the Conversation Playback uses
    # these bounds to play just the speech (`/audio/turn/{id}/user_speech.wav`).
    # `None` means the clip has no speech energy (a typed turn, or silence)
    # and the playback skips it; absent means no mic capture for the turn.
    for utt in utterances.values():
        utt_audio_files = utt.get("audio_files") or {}
        mic_kind = next((k for k in ("mic_raw", "mic_16k") if k in utt_audio_files), None)
        if mic_kind is None:
            continue
        bounds = speech_bounds(session_dir / utt_audio_files[mic_kind])
        utt["user_speech"] = (
            {"kind": mic_kind, "start_ms": bounds.start_ms, "end_ms": bounds.end_ms}
            if bounds
            else None
        )

    # Speech that made no turn: pair each `utterance_dropped` event with the
    # recording kept for it, in order (the event stream and the file names
    # count the same drops), so the console can play what the recogniser
    # heard nothing in. A recording with no event (a capture cut short, or
    # one from before the event was recorded) is still listed, from its name.
    for utt in utterances.values():
        recordings = sorted(utt.pop("dropped_recordings", []), key=lambda r: r["attempt"])
        if not recordings:
            continue
        for r in recordings:
            r["duration_ms"] = round(_wav_duration_ms(session_dir / r["file"]), 1)
        dropped = utt.setdefault("dropped_before", [])
        for i, r in enumerate(recordings):
            if i < len(dropped):
                dropped[i]["file"] = r["file"]
                dropped[i]["duration_ms"] = r["duration_ms"]
            else:
                dropped.append({
                    "reason": r["reason"],
                    "text": "",
                    "had_interim_text": False,
                    "audio_ms": round(r["duration_ms"]),
                    "at_ms": None,
                    "file": r["file"],
                    "duration_ms": r["duration_ms"],
                })

    # Calculate e2e_perceived_ms (end of user speech → first audio at the
    # client) from the best available anchor per turn — client VAD stamp ›
    # vp VAD stamp › provider anchor › cascade commit proxy — and say which
    # one was used (`e2e_anchor_source`). See resolve_speech_end_anchor.
    #
    # The chosen anchor also overwrites `_wallclock_ts_ms.user_audio_end_ms`
    # (+ `user_audio_end_source`) so `_flatten_turn` and the tool-call
    # attribution see the same instant. For S2S this is authoritative over
    # the `utterance_received` fallback above, which for S2S audio turns
    # fires at input-transcript *arrival* — often after the response audio
    # already started, which made the delta negative and suppressed E2E for
    # realtime sessions entirely.
    for utt in utterances.values():
        summary = utt.setdefault("summary", {})
        wc = utt.setdefault("_wallclock_ts_ms", {})
        anchor_ms, anchor_source = resolve_speech_end_anchor(utt)
        if anchor_ms is not None:
            wc["user_audio_end_ms"] = anchor_ms
            wc["user_audio_end_source"] = anchor_source
        first_out = wc.get("first_audio_out_ms")
        if anchor_ms is not None and isinstance(first_out, (int, float)):
            delta = first_out - anchor_ms
            if 0 <= delta <= _E2E_MAX_DELTA_MS:
                summary["e2e_perceived_ms"] = float(delta)
                summary["e2e_anchor_source"] = anchor_source
        if summary.get("e2e_perceived_ms") is None:
            # Headless / legacy clients never send `client_timing`
            # audio_receive reports, so the wallclock pair can't form. For an
            # anchored S2S turn the server-side `ttfb_ms` (+ the provider's
            # endpointing lag when vp stamped the end of speech itself) IS
            # the perceived E2E minus network delivery — surface it as an
            # estimate. Never for `turn_start` / unanchored turns: that ttfb
            # measures from turn creation (~0) and must never masquerade as
            # E2E (estimate_realtime_e2e enforces the gate).
            est = estimate_realtime_e2e(utt)
            if est is not None:
                summary["e2e_perceived_ms"], summary["e2e_anchor_source"] = est
                summary["e2e_estimated"] = True

    # Final pass: refresh result["utterances"] so any utterances created by the
    # matcher (e.g. legacy captures missing per-utterance JSON files) are included.
    if utterances:
        result["utterances"] = [utterances[k] for k in sorted(utterances)]

    # ── Client-side tools ──
    # Pair `client_tool_call` / `client_tool_result` trace events by
    # call_id into a session-level activity list: which tool the model
    # invoked, its arguments, and how the client resolved it. Each call
    # is then attributed to its chronological slot between turns
    # (`after_utterance_id`) so the session-detail turns view can render
    # the command card in conversation order instead of in a detached
    # panel at the top.
    result["client_tool_calls"] = _collect_client_tool_calls(all_events, _evt_ts_ms)
    _attribute_calls_to_turns(result["client_tool_calls"], result["utterances"])

    # Precomputed session metrics over the FULL event log.
    # Frontend prefers this over recomputing from the (paginated) `events` page.
    try:
        result["session_metrics"] = _compute_session_metrics(
            result["utterances"], all_events
        )
    except Exception:
        # The rollup is derived; the page shows the raw turns without it.
        log.warning("session metrics for %s could not be computed", session_id, exc_info=True)
        result["session_metrics"] = None

    return result

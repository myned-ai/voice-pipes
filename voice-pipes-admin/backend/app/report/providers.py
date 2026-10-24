"""Provider identity + device resolution for the `session.providers` block."""


# Streaming STT providers: `processing_ms` does not apply (transcript is
# emitted incrementally during the user's speech).  We expose a different
# metric (`stream_duration_ms`) and skip processing-time anomalies.
#
# The engine now records the mode in metadata.json (`stt_mode`, from the
# provider's declared Streaming capability), which takes precedence. This
# name set is the fallback for captures written before that field existed.
_STREAMING_STT_PROVIDERS: set[str] = {
    "assemblyai", "deepgram", "deepgram_streaming", "openai_realtime",
    "nemotron-streaming", "google",
}

_STT_MODES = {"streaming", "batch"}

# Speech-to-speech realtime providers: one model IS the whole STT+LLM+TTS
# stack (realtime/factory.rs canonical names + aliases). In these pipelines
# per-stage attribution is not meaningful — `llm.ttft_ms` is structurally
# absent and the model's entire response latency is recorded under
# `tts.ttfb_ms` — so the verdict must blame the model, not a "TTS" component
# that doesn't independently exist.
_S2S_REALTIME_PROVIDERS: set[str] = {
    "openai-realtime",
    "gemini-live", "gemini", "google-live",
    "elevenlabs", "elevenlabs-agents", "11labs",
}


def _provider_mode(provider: str | None, kind: str, declared: str | None = None) -> str:
    """Return `streaming` or `batch` for a given provider id.

    `declared` is the mode the engine recorded for the session (metadata.json
    `stt_mode`); when valid it wins over the provider-name fallback.
    """
    if isinstance(declared, str) and declared.lower() in _STT_MODES:
        return declared.lower()
    if not provider:
        return "unknown"
    p = provider.lower()
    if kind == "stt":
        return "streaming" if p in _STREAMING_STT_PROVIDERS else "batch"
    # LLM/TTS: default to streaming when the worker emitted `stream: true`
    # in the corresponding detail payload.  Caller fills that in.
    return "unknown"


def _coerce_device(raw) -> dict | None:
    """Normalise a persisted `*_device` block to `{kind, detail?, source?}`.

    Returns ``None`` for anything without a usable ``kind`` (absent key, JSON
    ``null``, or a malformed object) so that captures predating device capture
    keep a byte-identical `providers` block — the device key is simply omitted.
    """
    if not isinstance(raw, dict):
        return None
    kind = raw.get("kind")
    if not kind:
        return None
    out = {"kind": kind}
    if raw.get("detail"):
        out["detail"] = raw["detail"]
    if raw.get("source"):
        out["source"] = raw["source"]
    return out


def _extract_provider_identity(utterances: list[dict], metadata: dict) -> dict:
    """Build the `session.providers` block.  Pulls the canonical model/voice
    from the richest LLM/STT/TTS payload available across the session."""
    llm_provider = None
    llm_model = None
    llm_stream = None
    stt_provider = (metadata.get("stt_provider") or "").lower() or None
    tts_provider = (metadata.get("tts_provider") or "").lower() or None
    tts_voice = None
    # The model + version the TTS *process* actually loaded — emitted in
    # tts_detail by the engine after it probes the sidecar's /v1/models at
    # warm-up. Lets the report show e.g. "VibeVoice-Realtime-0.5B" instead of a
    # bare "openai" even though TTS runs in a separate process.
    tts_model = None
    tts_model_version = None

    for utt in utterances:
        ld = utt.get("llm_detail") or {}
        if ld:
            llm_model = llm_model or ld.get("model")
            llm_stream = llm_stream if llm_stream is not None else ld.get("stream")
        lt = utt.get("llm_timing") or {}
        llm_provider = llm_provider or lt.get("provider") or ld.get("provider")
        td = utt.get("tts_detail") or {}
        tt = utt.get("tts_timing") or {}
        tts_voice = tts_voice or td.get("voice")
        tts_provider = tts_provider or td.get("provider") or tt.get("provider")
        tts_model = tts_model or td.get("model") or tt.get("model")
        tts_model_version = tts_model_version or td.get("model_version") or tt.get("model_version")
        st = utt.get("stt_timing") or {}
        stt_provider = stt_provider or st.get("provider")

    # Resolved-at-setup model identities, written to metadata.json by the engine
    # (`{stt,llm,tts}_model = {id, version, source}`). This is the PRIMARY source
    # — the model actually running, even when the provider is a separate process.
    # The event-scraped values above are the fallback for older captures.
    stt_meta = metadata.get("stt_model") or {}
    llm_meta = metadata.get("llm_model") or {}
    tts_meta = metadata.get("tts_model") or {}
    # The voice resolved at session setup is the primary source; trace events
    # only carry it for providers whose detail events reach the session log.
    tts_voice = metadata.get("tts_voice") or tts_voice

    # Resolved-at-setup compute device per component, written to metadata.json as
    # `{stt,llm,tts}_device = {kind, detail?, source?}`. Added to the block only
    # when present so legacy captures are unaffected.
    llm: dict = {
        "provider": llm_provider,
        "model": llm_meta.get("id") or llm_model,
        "model_version": llm_meta.get("version"),
        "stream": llm_stream,
    }
    stt: dict = {
        "provider": stt_provider,
        "mode": _provider_mode(stt_provider, "stt", metadata.get("stt_mode")),
        "model": stt_meta.get("id"),
        "model_version": stt_meta.get("version"),
    }
    tts: dict = {
        "provider": tts_provider,
        "voice": tts_voice,
        "model": tts_meta.get("id") or tts_model,
        "model_version": tts_meta.get("version") or tts_model_version,
    }
    for block, key in ((llm, "llm_device"), (stt, "stt_device"), (tts, "tts_device")):
        device = _coerce_device(metadata.get(key))
        if device is not None:
            block["device"] = device

    out = {"llm": llm, "stt": stt, "tts": tts}
    if _is_s2s(llm, stt, tts):
        out["s2s"] = True
    turn_detection = _coerce_turn_detection(metadata.get("turn_detection"))
    if turn_detection is not None:
        out["turn_detection"] = turn_detection
    return out


def _coerce_turn_detection(raw) -> dict | None:
    """How the session decided a turn had ended, from `metadata.json`.

    `silence_timeout_ms` is the floor under `endpointing_ms`: no turn can
    commit sooner than that after the speaker stops. It is what lets the
    report tell a wait that was configured from one the recogniser caused.
    Absent on captures written before the engine recorded it, and then the
    report makes no claim either way rather than guessing a floor.
    """
    if not isinstance(raw, dict):
        return None
    timeout = raw.get("silence_timeout_ms")
    if not isinstance(timeout, (int, float)) or isinstance(timeout, bool) or timeout < 0:
        return None
    mode = raw.get("mode")
    return {
        "mode": mode if isinstance(mode, str) and mode else "silence",
        "silence_timeout_ms": float(timeout),
    }


def _is_s2s(llm: dict, stt: dict, tts: dict) -> bool:
    """True when the session ran on a single speech-to-speech model.

    Primary signal: STT and TTS resolve to the same known realtime provider.
    Structural fallback (provider name missing or unknown): all three
    components report the same non-null model id — no independent STT/LLM/TTS
    stack produces that shape.
    """
    stt_p, tts_p = stt.get("provider"), tts.get("provider")
    if stt_p and stt_p == tts_p and stt_p in _S2S_REALTIME_PROVIDERS:
        return True
    models = {llm.get("model"), stt.get("model"), tts.get("model")}
    return len(models) == 1 and None not in models

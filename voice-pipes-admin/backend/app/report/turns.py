"""Per-turn flattening + the model-agnostic critical-path decomposition.

`_flatten_turn` reshapes one merged capture utterance into the LLM-friendly
per-turn row; `critical_path_latency` estimates the server-side wait to first
audio; `_stt_critical_ms` bounds how much of STT is actually on that path.
`_apply_tool_context` overlays client-side tool calls onto the flattened rows:
it classifies tool legs and re-anchors the followup turn's TTFB at the
tool-result arrival so a slow client tool is not misattributed to TTS.
"""

from datetime import datetime

from ..captures.reader import (
    INPUT_AUDIO_KEYS,
    _normalize_llm_payload,
    _num,
    arrival_rtf,
    project_snapshot_metrics,
    project_throughput_metrics,
)


def _playback_block(utt: dict) -> dict | None:
    """Client playback health (the persisted `playback_quality` frame) as a
    per-turn row block. None when the client sent no frame (headless / legacy
    captures). `arrival_rtf` (wall first→last frame received ÷ audio) is the
    server-vs-network discriminator behind `client.starvation` / `client.jitter`
; the annotation_late_* pair is the
    client-side truth behind `annotator.lagging`."""
    ph = utt.get("playback_health")
    if not isinstance(ph, dict) or not ph:
        return None
    return {
        "underruns": int(_num(ph.get("underrun_count")) or 0),
        "total_silence_ms": _num(ph.get("total_silence_ms")),
        "audio_ms": _num(ph.get("audio_ms")),
        "wall_ms": _num(ph.get("wall_ms")),
        "arrival_wall_ms": _num(ph.get("arrival_wall_ms")),
        "arrival_rtf": arrival_rtf(ph),
        "interrupted": bool(ph.get("interrupted")) if ph.get("interrupted") is not None else None,
        "annotation_late_count": int(_num(ph.get("annotation_late_count")) or 0),
        "annotation_late_ms_max": _num(ph.get("annotation_late_ms_max")),
    }


def _stt_critical_ms(stt: dict) -> float:
    """How much of STT is on the user-facing critical path (ms), *beyond* the
    endpointing stage.

    - streaming STT: 0 — the transcript arrives *during* the user's speech.
    - batch STT with ``endpointing_ms`` present: 0. In this pipeline the
      turn-commit is gated on the final transcript (pipeline/stt.rs — both
      commit paths require a final STT result before the utterance is sent),
      and ``endpointing_ms`` is measured *at the commit decision*
      (``last_speech_instant.elapsed()``), so any post-speech wait for the
      transcript is already inside the endpointing stage. ``processing_ms``
      is audio-start → final-transcript wall time — it spans the whole
      utterance, and charging it (or its rtfx-scaled compute bound) on top of
      endpointing double-counts. On long utterances that double-count alone
      pinned sessions to "broken" (e.g. a 10s utterance → processing 10284ms,
      rtfx 2.07 → ~5s charged to the path while the observed commit →
      llm_request gap was ~1ms).
    - batch STT without ``endpointing_ms`` (legacy captures): keep the
      conservative overlap bound — ``processing_ms / rtfx`` when ``rtfx > 1``
      (compute wall-time, an upper bound on the post-speech tail), else the
      full ``processing_ms``.
    """
    if (stt.get("mode") or "").lower() == "streaming":
        return 0.0
    proc = _num(stt.get("processing_ms"))
    if proc is None or proc <= 0:
        return 0.0
    if _num(stt.get("endpointing_ms")) is not None:
        return 0.0
    rtfx = _num(stt.get("rtfx"))
    if rtfx is not None and rtfx > 1.0:
        return proc / rtfx
    return proc


def _llm_has_work(llm: dict) -> bool:
    """Whether the LLM component reported generation/prefill work on this turn."""
    for k in (
        "prompt_ingest_ms",
        "generate_ms",
        "generation_ms",
        "perceived_ttft_ms",
        "completion_tokens",
        "out_chars",
    ):
        v = _num(llm.get(k))
        if v is not None and v > 0:
            return True
    return False


def _usable_ttft_ms(llm: dict) -> float | None:
    """Return `llm.ttft_ms` only when it is a trustworthy measurement.

    A 0 ms TTFT alongside positive LLM work (`prompt_ingest_ms`, `generate_ms`,
    `generation_ms`, or `completion_tokens`) is a measurement fault
    (`measurement.zero_with_work`), not an instantaneous completion.
    Likewise, a TTFT smaller than `prompt_ingest_ms` contradicts its own
    prefill breakdown.
    """
    ttft = _num(llm.get("ttft_ms"))
    if ttft is None:
        return None
    if ttft <= 0 and _llm_has_work(llm):
        return None
    prompt_ingest = _num(llm.get("prompt_ingest_ms"))
    if prompt_ingest is not None and prompt_ingest > 0 and ttft < prompt_ingest:
        return None
    return ttft


def _estimated_llm_wait_ms(llm: dict, tts: dict) -> float | None:
    """When `ttft_ms` is unusable, estimate the LLM critical-path wait from
    `perceived_ttft_ms` or `llm_detail` (`prompt_ingest_ms` + time to first
    sentence when commit-anchored `internal_ttfb_ms` is valid)."""
    perceived = _num(llm.get("perceived_ttft_ms"))
    if perceived is not None and perceived > 0:
        return perceived
    prompt_ingest = _num(llm.get("prompt_ingest_ms"))
    if prompt_ingest is not None and prompt_ingest > 0:
        ttfb = _num(tts.get("ttfb_ms"))
        internal_ttfb = _num(tts.get("internal_ttfb_ms"))
        if (
            ttfb is not None
            and internal_ttfb is not None
            and 0 < internal_ttfb <= ttfb
            and (ttfb - internal_ttfb) >= prompt_ingest
        ):
            return ttfb - internal_ttfb
        return prompt_ingest
    return None


def resolve_turn_ttft(
    snapshot_ttft: float | None,
    llm_detail: dict | None,
    *,
    generation_ms: float | None = None,
) -> tuple[float | None, bool]:
    """Resolve turn `ttft_ms` and `ttft_estimated` flag (shared by `turns.py`
    and `routers/sessions.py`).

    Falls back to `llm_detail.first_token_ms` only when `snapshot_ttft` is
    `None` and `first_token_ms` is positive and not smaller than
    `prompt_ingest_ms`. Preserves a raw `0.0` `snapshot_ttft` untouched so
    `_detect_anomalies` can flag `measurement.zero_with_work`.
    """
    ttft = _num(snapshot_ttft)
    if ttft is not None:
        return ttft, False
    detail = llm_detail or {}
    fb = _num(detail.get("first_token_ms"))
    if fb is None:
        return None, False
    prompt_ingest = _num(detail.get("prompt_ingest_ms"))
    gen = _num(detail.get("generate_ms"))
    if gen is None:
        gen = _num(generation_ms)
    has_work = (prompt_ingest is not None and prompt_ingest > 0) or (
        gen is not None and gen > 0
    )
    if fb <= 0 and has_work:
        return None, False
    if prompt_ingest is not None and prompt_ingest > 0 and fb < prompt_ingest:
        return None, False
    return fb, True


def critical_path_latency(turn: dict) -> dict:
    """Server-side estimate of the critical-path latency to first audio (ms).

    A model-agnostic decomposition of the sequential critical path into
    *exclusive* stages:
      ``endpointing`` (last speech → commit, includes any wait for the final
      transcript) → ``stt_critical`` (post-commit STT tail; 0 in this
      pipeline) → ``ttft`` (commit → first LLM token) → ``tts_ttfb``
      (first token → first audio).

    The canonical ``ttfb_ms`` is the runner-observed value — utterance-commit
    → first non-empty audio chunk forwarded to the client
    (transport/serve/turn.rs) — so it already *contains* ``ttft``. When the
    runner stamped it, the pipeline-internal value is preserved under
    ``tts_internal_ttfb_ms``; its presence marks the commit anchor, and the
    TTS stage is then ``ttfb - ttft`` instead of the raw ``ttfb`` (adding
    ``ttft + ttfb`` would double-count the LLM wait — on a turn with an
    873ms LLM connect stall that alone inflated the estimate by ~1.1s).

    When ``ttft_ms`` is unusable (0 alongside LLM work, or missing), the
    commit-anchored wait is attributed using ``perceived_ttft_ms`` /
    ``llm_detail.prompt_ingest_ms`` when present, else left unattributed —
    never charged to TTS by subtracting 0.
    """
    stt = turn.get("stt") or {}
    llm = turn.get("llm") or {}
    tts = turn.get("tts") or {}
    output_block = turn.get("output") or {}
    endpointing_raw = _num(stt.get("endpointing_ms"))
    endpointing = endpointing_raw if endpointing_raw is not None else 0.0
    stt_critical = _stt_critical_ms(stt)

    llm_wait_raw = _num(llm.get("llm_wait_ms"))
    tts_wait_raw = _num(tts.get("tts_wait_ms"))
    output_wait_raw = _num(tts.get("output_wait_ms"))
    if output_wait_raw is None:
        output_wait_raw = _num(output_block.get("output_wait_ms"))
    commit_to_first_audio_raw = _num(tts.get("commit_to_first_audio_ms"))
    if commit_to_first_audio_raw is None:
        commit_to_first_audio_raw = _num(output_block.get("commit_to_first_audio_ms"))
    has_engine_stages = any(
        v is not None
        for v in (
            llm_wait_raw,
            tts_wait_raw,
            output_wait_raw,
            commit_to_first_audio_raw,
        )
    )

    unattributed = 0.0
    output_stage = 0.0
    stages_disagree_ms: float | None = None
    if has_engine_stages:
        llm_stage = (
            llm_wait_raw
            if llm_wait_raw is not None and (llm_wait_raw > 0 or not _llm_has_work(llm))
            else (_usable_ttft_ms(llm) or _estimated_llm_wait_ms(llm, tts) or 0.0)
        )
        tts_stage = (
            tts_wait_raw
            if tts_wait_raw is not None
            else (0.0 if tts.get("stage_skipped_reason") else (_num(tts.get("ttfb_ms")) or 0.0))
        )
        output_stage = output_wait_raw if output_wait_raw is not None else 0.0
        stage_sum = llm_stage + tts_stage + output_stage
        if commit_to_first_audio_raw is not None:
            post_commit = commit_to_first_audio_raw
            diff = abs(stage_sum - commit_to_first_audio_raw)
            if (
                diff > 50.0
                and commit_to_first_audio_raw > 0
                and (diff / commit_to_first_audio_raw) > 0.05
            ):
                stages_disagree_ms = round(stage_sum - commit_to_first_audio_raw, 1)
                if commit_to_first_audio_raw > stage_sum:
                    unattributed = commit_to_first_audio_raw - stage_sum
        else:
            post_commit = stage_sum
        total = endpointing + stt_critical + post_commit
    else:
        ttfb_raw = _num(tts.get("ttfb_ms"))
        ttfb = ttfb_raw if ttfb_raw is not None else 0.0
        internal_ttfb = _num(tts.get("internal_ttfb_ms"))
        commit_anchored = internal_ttfb is not None

        usable_ttft = _usable_ttft_ms(llm)
        if usable_ttft is not None:
            llm_stage = usable_ttft
            if commit_anchored:
                tts_stage = max(ttfb - llm_stage, 0.0) if ttfb_raw is not None else 0.0
            else:
                tts_stage = ttfb
        else:
            llm_est = _estimated_llm_wait_ms(llm, tts)
            if llm_est is not None:
                llm_stage = llm_est
                if commit_anchored:
                    tts_stage = max(ttfb - llm_stage, 0.0) if ttfb_raw is not None else 0.0
                else:
                    tts_stage = ttfb
            elif commit_anchored and _llm_has_work(llm):
                # P1.2: TTFT is unusable and no LLM breakdown is available to split
                # the commit-anchored wait. Never charge the whole wait to TTS by
                # subtracting 0: use `internal_ttfb_ms` when valid, and leave the
                # rest unattributed.
                llm_stage = 0.0
                if ttfb_raw is not None and 0.0 <= internal_ttfb <= ttfb:
                    tts_stage = internal_ttfb
                    unattributed = ttfb - internal_ttfb
                else:
                    tts_stage = 0.0
                    unattributed = ttfb
            else:
                llm_stage = 0.0
                tts_stage = ttfb
        total = endpointing + stt_critical + llm_stage + tts_stage + unattributed

    stages = {
        "endpointing_ms": round(endpointing, 1),
        "stt_critical_ms": round(stt_critical, 1),
        "llm_ttft_ms": round(llm_stage, 1),
        "tts_ttfb_ms": round(tts_stage, 1),
    }
    if has_engine_stages:
        stages["llm_wait_ms"] = round(llm_stage, 1)
        stages["tts_wait_ms"] = round(tts_stage, 1)
        stages["output_wait_ms"] = round(output_stage, 1)
    # Dominant stage → a model-agnostic culprit hint for the experience tier.
    if total > 0:
        cand_keys: tuple[str, ...] = (
            ("endpointing_ms", "stt_critical_ms", "llm_ttft_ms", "tts_ttfb_ms", "output_wait_ms")
            if has_engine_stages
            else ("endpointing_ms", "stt_critical_ms", "llm_ttft_ms", "tts_ttfb_ms")
        )
        best_key = max(cand_keys, key=lambda k: stages[k])
        dominant = best_key if stages[best_key] >= unattributed and stages[best_key] > 0 else None
    else:
        dominant = None
    out = {
        **stages,
        "total_ms": round(total, 1) if total > 0 else None,
        "dominant_stage": dominant,
    }
    if has_engine_stages and commit_to_first_audio_raw is not None:
        out["commit_to_first_audio_ms"] = round(commit_to_first_audio_raw, 1)
    if stages_disagree_ms is not None:
        out["stages_disagree_ms"] = stages_disagree_ms
    if unattributed > 0:
        out["unattributed_ms"] = round(unattributed, 1)
    return out


def _flatten_turn(utt: dict, t0_iso: str | None, first_utt_id, providers: dict) -> dict:
    """Flatten one utterance into the LLM-friendly per-turn row.

    `utt` is the merged dict produced by `_read_capture_artifacts` —
    i.e. it carries `summary`, `stt`, `llm_input`, `llm_output`,
    `llm_timing`, `llm_detail`, `tts_timing`, `tts_detail`, plus
    barge-in fields.  `providers` is the resolved
    `{llm:{provider,model,stream}, stt:{provider,mode}, tts:{...}}` block
    so we can apply provider-mode-aware logic (streaming STT vs batch).
    """
    s = utt.get("summary") or {}
    llm_detail = utt.get("llm_detail") or {}
    llm_norm = _normalize_llm_payload(llm_detail or utt.get("llm_timing") or {})
    tts_payload = utt.get("tts_detail") or utt.get("tts_timing") or {}
    stt_payload = utt.get("stt_timing") or {}

    # Cold-start detection: the lowest utterance_id in the session.  Captures
    # use 0-based ids in some configs and 1-based in others, so we don't
    # hard-code the value — it's resolved by the caller.
    is_first_turn = utt.get("utterance_id") == first_utt_id

    stt_text = (utt.get("stt") or {}).get("text") or utt.get("user_text") or utt.get("final_transcript") or ""
    llm_in = utt.get("llm_input") or ""
    llm_out = utt.get("llm_output") or {}
    llm_out_text = llm_out.get("text") or utt.get("retained_text") or utt.get("agent_text") or ""
    spoken_text = llm_out.get("spoken_text") or utt.get("spoken_text") or None

    stt_mode = (providers.get("stt") or {}).get("mode") or "unknown"
    # The configured silence wait, when the capture recorded it. Cascade only:
    # on a speech-to-speech turn `endpointing_ms` is the provider's own lag,
    # which no setting of ours puts a floor under.
    silence_timeout_ms = (
        None if providers.get("s2s")
        else (providers.get("turn_detection") or {}).get("silence_timeout_ms")
    )

    # Canonical per-turn latencies come from the metric snapshot (single source
    # of truth); lifecycle/barge-in fields stay on the summary. See
    # captures.reader.project_snapshot_metrics.
    proj = project_snapshot_metrics(utt.get("metric_snapshot"))

    # ── STT block: streaming providers don't have a meaningful
    # `processing_ms` (transcript arrives during user speech).  Surface
    # `stream_duration_ms` (audio_start → final transcript wall-time)
    # instead and leave processing_ms null.
    stt_block: dict = {
        "mode": stt_mode,
        # Floor under `endpointing_ms` (null when unknown). See
        # `anomalies._endpointing_attribution`.
        "silence_timeout_ms": silence_timeout_ms,
        # Cascade: vp's VAD silence wait at commit. S2S: the provider's own
        # endpointing lag (`provider_endpointing_ms` = provider anchor − vp's
        # uplink-VAD end of speech) — the same user-felt "nobody noticed I
        # stopped" stage, so it feeds `critical_path.endpointing_ms` and
        # `stt.endpointing_high` alike.
        "endpointing_ms": (
            proj["endpointing_ms"] if proj["endpointing_ms"] is not None
            else proj["provider_endpointing_ms"]
        ),
        "chars": len(stt_text) if stt_text else None,
        "cold_start": is_first_turn or None,
        # RTFx (audio/wall) — the overlap signal the critical-path-latency model
        # uses to decide how much of processing_ms is on the critical path.
        "rtfx": proj.get("stt_rtfx"),
        # Audio the recogniser was given for the turn, which is how long the
        # caller was heard speaking -- longer than the client thought when
        # its detector took a pause for the end.
        "audio_duration_ms": proj.get("stt_audio_duration_ms"),
    }
    if stt_mode == "streaming":
        stt_block["processing_ms"] = None
        stt_block["stream_duration_ms"] = stt_payload.get("elapsed_ms")
    else:
        stt_block["processing_ms"] = (
            proj["processing_ms"] if proj["processing_ms"] is not None
            else stt_payload.get("elapsed_ms")
        )

    # ── LLM block: shared TTFT resolution (P1.7).
    ttft, ttft_estimated = resolve_turn_ttft(
        proj["ttft_ms"], llm_detail, generation_ms=proj["generation_ms"]
    )
    gen_ms = (
        proj["generation_ms"]
        if proj["generation_ms"] is not None and proj["generation_ms"] > 0
        else llm_norm.get("generate_ms")
    )

    llm_block: dict = {
        "ttft_ms": ttft,
        "ttft_estimated": ttft_estimated or None,
        # `connect_ms` is emitted by the openai-compat streaming path (absent for
        # local / non-network providers). `first_token_ms` is the provider-reported
        # request-start → first-token breakdown from `llm_detail` (both include
        # connect on hosted providers; `first_token_ms` is NOT `TTFT - connect`).
        "connect_ms": (
            llm_norm.get("connect_ms")
            if llm_norm.get("connect_ms") is not None
            else llm_detail.get("connect_ms")
        ),
        "first_token_ms": (
            llm_norm.get("first_token_ms")
            if llm_norm.get("first_token_ms") is not None
            else llm_detail.get("first_token_ms")
        ),
        "generation_ms": gen_ms,
        "prompt_tokens": llm_norm.get("prompt_tokens"),
        "completion_tokens": llm_norm.get("output_tokens"),
        "tokens_per_sec": (
            proj.get("tokens_per_sec")
            if proj.get("tokens_per_sec") is not None
            else llm_norm.get("tokens_per_sec")
        ),
        "decode_ms": (
            proj.get("decode_ms")
            if proj.get("decode_ms") is not None
            else llm_norm.get("decode_ms")
        ),
        "model": llm_norm.get("model") or (providers.get("llm") or {}).get("model"),
        "in_chars": len(llm_in) if llm_in else None,
        "out_chars": len(llm_out_text) if llm_out_text else None,
        "cold_start": is_first_turn or None,
    }
    if proj.get("llm_perceived_ttft_ms") is not None:
        llm_block["perceived_ttft_ms"] = proj["llm_perceived_ttft_ms"]
    if proj.get("llm_wait_ms") is not None:
        llm_block["llm_wait_ms"] = proj["llm_wait_ms"]
    if proj.get("llm_stage_skipped_reason") is not None:
        llm_block["stage_skipped_reason"] = proj["llm_stage_skipped_reason"]
    if llm_norm.get("prompt_ingest_ms") is not None:
        llm_block["prompt_ingest_ms"] = llm_norm["prompt_ingest_ms"]

    # ── TTS block: largely unchanged; prefer detail.voice over the
    # session-level fallback.
    tts_block: dict = {
        "ttfb_ms": proj["ttfb_ms"],
        # Pipeline-internal pre-pacer TTFB (first LLM fragment → first audio
        # sample). Present iff the runner overwrote the canonical `ttfb_ms`
        # with the commit-anchored perceived value on legacy captures — its
        # presence (without `commit_to_first_audio_ms`) is the signal that
        # `ttfb_ms` contains LLM TTFT (see critical_path_latency).
        "internal_ttfb_ms": proj["tts_internal_ttfb_ms"],
        "synthesis_ms": (
            proj["synthesis_ms"]
            if proj["synthesis_ms"] is not None
            else tts_payload.get("synthesis_ms")
        ),
        "audio_duration_ms": (
            proj["audio_duration_ms"]
            if proj["audio_duration_ms"] is not None
            else (
                int((tts_payload.get("audio_duration_s") or 0) * 1000)
                if tts_payload.get("audio_duration_s") is not None
                else None
            )
        ),
        "rtf": proj.get("rtf") if proj.get("rtf") is not None else tts_payload.get("rtf"),
        "chars": (
            proj.get("chars")
            if proj.get("chars") is not None
            else (tts_payload.get("chars") or (len(llm_out_text) if llm_out_text else None))
        ),
        "voice": tts_payload.get("voice") or (providers.get("tts") or {}).get("voice"),
        "cold_start": is_first_turn or None,
    }
    if proj.get("tts_wait_ms") is not None:
        tts_block["tts_wait_ms"] = proj["tts_wait_ms"]
    if proj.get("output_wait_ms") is not None:
        tts_block["output_wait_ms"] = proj["output_wait_ms"]
    if proj.get("commit_to_first_audio_ms") is not None:
        tts_block["commit_to_first_audio_ms"] = proj["commit_to_first_audio_ms"]
    if proj.get("tts_stage_skipped_reason") is not None:
        tts_block["stage_skipped_reason"] = proj["tts_stage_skipped_reason"]

    # Perceived end-to-end wait.  Prefer the wall-clock metric
    # `first_audio_out_ms - user_audio_end_ms` (item 6 of the
    # diagnostic-metrics plan) — this captures the user's true wait
    # from "stopped speaking" to "first audio reaches client".
    # Fall back to a sum-of-components estimate when the events are
    # missing or skewed; tag with `e2e_estimated: true` in that case.
    e2e: float | None = None
    e2e_source = "estimated"
    # Which end-of-speech instant the E2E is anchored on (reader's
    # resolve_speech_end_anchor precedence: client_speech_end › vp_vad ›
    # provider anchor › stt_timing / utterance_received). For an estimated
    # S2S E2E the reader's estimate (ttfb [+ provider endpointing]) is the
    # same sum this function builds below, so its anchor label carries over.
    e2e_anchor_source: str | None = None
    wc = utt.get("_wallclock_ts_ms") or {}
    user_end = wc.get("user_audio_end_ms")
    first_out = wc.get("first_audio_out_ms")
    if isinstance(user_end, (int, float)) and isinstance(first_out, (int, float)):
        delta = first_out - user_end
        # Sanity bound: clock skew or mis-attribution can produce
        # negative or absurdly large gaps; fall through to the estimate.
        if 0 <= delta <= 60_000:
            e2e = float(delta)
            e2e_source = "wallclock"
            e2e_anchor_source = wc.get("user_audio_end_source")
    if e2e is None and s.get("e2e_estimated"):
        e2e_anchor_source = s.get("e2e_anchor_source")

    if e2e is None:
        # P1.4 (S12, P5): Sum-of-components fallback without double counting.
        # - Never add batch `processing_ms` on top of `endpointing_ms` (the turn
        #   commit is gated on the final transcript, so the post-speech STT tail
        #   is already inside `endpointing_ms`).
        # - Prefer `commit_to_first_audio_ms` when present; on legacy captures,
        #   never add `ttft_ms` on top of a commit-anchored `tts.ttfb_ms`
        #   (`internal_ttfb_ms` present).
        parts: list[float] = []
        if isinstance(stt_block.get("endpointing_ms"), (int, float)):
            parts.append(float(stt_block["endpointing_ms"]))
        elif stt_mode != "streaming" and isinstance(stt_block.get("processing_ms"), (int, float)):
            parts.append(_stt_critical_ms(stt_block))

        commit_span = _num(tts_block.get("commit_to_first_audio_ms"))
        commit_anchored = tts_block.get("internal_ttfb_ms") is not None
        if commit_span is not None:
            parts.append(commit_span)
        elif commit_anchored and isinstance(tts_block.get("ttfb_ms"), (int, float)):
            parts.append(float(tts_block["ttfb_ms"]))
        else:
            usable_ttft = _usable_ttft_ms(llm_block)
            if usable_ttft is not None:
                parts.append(float(usable_ttft))
            if isinstance(tts_block["ttfb_ms"], (int, float)):
                parts.append(float(tts_block["ttfb_ms"]))
        e2e = sum(parts) if parts else None

    row: dict = {
        "utt": utt.get("utterance_id"),
        # "user" | "agent" — agent-initiated turns (idle prompts, filler
        # follow-ups, proactive speech) have no user input: latencies are
        # anchored at turn-open, there is no STT block, and consumers must
        # not fold them into user-perceived latency statistics.
        "initiator": proj.get("initiator") or "user",
        "interrupted": bool(s.get("interrupted")),
        "terminated": bool(s.get("terminated")),
        "stt": stt_block,
        "llm": llm_block,
        "tts": tts_block,
        "total_ms": proj["total_ms"],
        "e2e_perceived_ms": e2e,
        "e2e_estimated": e2e_source != "wallclock",
        "e2e_anchor_source": e2e_anchor_source,
        "user_transcript": stt_text or None,
        "agent_transcript": llm_out_text or None,
        "spoken_text": spoken_text or None,
    }
    # Speech under this id that was thrown away before this turn committed.
    # Absent when nothing was: a key that is always there reads as a field to
    # fill, and this one is an event.
    dropped = utt.get("dropped_before")
    if dropped:
        row["dropped_before"] = dropped
    # Experience tier: model-agnostic critical-path-latency decomposition. Additive;
    # nothing reads it for the verdict until a later phase.
    row["critical_path"] = critical_path_latency(row)

    # Anchor provenance: where the
    # turn's latencies are measured from. Present on realtime turns and on
    # any turn the client stamped with a VAD `speech_end`; absent on cascade
    # turns without one (old captures render nothing, not a wall of nulls).
    #   ttfb_anchor_source       — what `tts.ttfb_ms` starts from
    #                              (`provider_speech_stopped`,
    #                              `input_transcript_proxy`, `vp_vad`,
    #                              `client_speech_end`, `text_submit`;
    #                              `provider` on legacy anchored captures;
    #                              `turn_start` = NO anchor, ttfb ≈ 0)
    #   first_text_ms            — anchor → first output transcript (diagnostic)
    #   provider_endpointing_ms  — provider anchor − vp VAD end of speech
    #   vp_speech_end_epoch_ms / client_speech_end_epoch_ms — the stamps
    #   e2e_anchor_source        — what `e2e_perceived_ms` starts from
    anchor: dict = {}
    if proj.get("realtime"):
        anchor = {
            "ttfb_anchor_source": proj["ttfb_anchor_source"],
            "first_text_ms": proj["first_text_ms"],
            "provider_endpointing_ms": proj["provider_endpointing_ms"],
            "vp_speech_end_epoch_ms": proj["vp_speech_end_epoch_ms"],
        }
    client_end = wc.get("client_speech_end_ms")
    if isinstance(client_end, (int, float)):
        anchor["client_speech_end_epoch_ms"] = float(client_end)
    if anchor:
        anchor["e2e_anchor_source"] = e2e_anchor_source
        row["anchor"] = anchor

    # Input audio quality: the user's speech as
    # the server received it — `speech_dbfs`, `peak_dbfs`, `noise_dbfs`,
    # `snr_db`, `clipped_ratio`, `speech_ratio`, `speech_ms`, `level_source`
    # (the wire keys minus the `input_` prefix). Absent on agent-initiated
    # turns and on captures without the keys. Not judged yet: thresholds
    # arrive with a later calibration pass.
    if proj.get("input_speech_dbfs") is not None and row["initiator"] != "agent":
        row["input_audio"] = {
            key.removeprefix("input_"): proj[key] for key in INPUT_AUDIO_KEYS
        }

    # Streaming-phase throughput:
    #   output     — emit_rtf (post-chain wall ÷ audio), source_rtf (what the
    #                provider/TTS delivered on its own), chain_overhead_ms
    #                (post-processors only), pacer_wait_ms (the rest);
    #   audio_post — per inline post-processor: p50 / budget_ratio / timeouts;
    #   annotators — per off-path annotator (metadata only, never audio);
    #   playback   — the client's frame incl. arrival_rtf.
    # Blocks are only present when the capture carries them, so old captures
    # (no `output` component) and headless clients (no playback frame) render
    # nothing rather than a wall of nulls.
    thr = project_throughput_metrics(utt.get("metric_snapshot"))
    if thr["output"] is not None:
        row["output"] = thr["output"]
    if thr["audio_post"]:
        row["audio_post"] = thr["audio_post"]
    if thr["annotators"]:
        row["annotators"] = thr["annotators"]
    playback = _playback_block(utt)
    if playback is not None:
        row["playback"] = playback

    if s.get("interrupted"):
        row["bi"] = {
            "trigger": s.get("interrupt_trigger"),
            "played_ms": s.get("last_played_offset_ms"),
            "barge_in_rtt_ms": s.get("barge_in_rtt_ms"),
            "retained_chars": len(utt.get("retained_text") or "") or None,
        }

    if s.get("timestamp") and t0_iso:
        # `timestamp` is epoch ms; convert to relative seconds from t0.
        try:
            t0 = datetime.fromisoformat(t0_iso.replace("Z", "+00:00"))
            row["t_offset_s"] = round(
                (s["timestamp"] / 1000.0) - t0.timestamp(), 2
            )
        except (AttributeError, TypeError, ValueError):
            # Unparseable timestamps: the turn has no offset.
            pass

    return row


# ── Client-side tool context ──
#
# A tool exchange spans two turn rows in the capture:
#   - the *invocation* leg: the model emits a function call instead of (or in
#     addition to) speech — no agent audio when speech was withheld;
#   - the *followup* leg: after the client resolves the call, the model speaks.
# The followup's canonical `tts.ttfb_ms` is commit-anchored, and its commit
# predates the client-tool round-trip — so the raw value contains the entire
# tool wait and, uncorrected, books it against TTS (a 7.4s `play_media` call
# once graded a healthy gpt-realtime session as "broken — TTS unstable").

def tool_roundtrip_ms(call: dict) -> float | None:
    """Dispatch → client-result wall time for one resolved tool call (ms).
    This is APPLICATION-domain time (the client executing the action, or a
    human resolving the playground card) — informational, not a pipeline
    latency; see anomalies._tool_call_anomalies."""
    a, b = call.get("called_at_ms"), call.get("resolved_at_ms")
    if isinstance(a, (int, float)) and isinstance(b, (int, float)) and b >= a:
        return float(b) - float(a)
    return None


def tool_ack_ms(call: dict) -> float | None:
    """Dispatch → first `started`/`progress` ack from the client (ms) —
    the integration-responsiveness signal, distinct from execution time."""
    a, b = call.get("called_at_ms"), call.get("acked_at_ms")
    if isinstance(a, (int, float)) and isinstance(b, (int, float)) and b >= a:
        return float(b) - float(a)
    return None


def _compact_tool_call(call: dict) -> dict:
    """Per-turn projection of a tool-call row: identity + outcome + latency.
    Arguments/results stay on the session-level `client_tool_calls` list."""
    out = {
        "call_id": call.get("call_id"),
        "name": call.get("name"),
        "status": call.get("status"),
    }
    rt = tool_roundtrip_ms(call)
    if rt is not None:
        out["roundtrip_ms"] = round(rt, 1)
    ack = tool_ack_ms(call)
    if ack is not None:
        out["ack_ms"] = round(ack, 1)
    for k in ("speak_on_result", "spoken"):
        if call.get(k) is not None:
            out[k] = call[k]
    return out


def _apply_tool_context(
    flat_turns: list[dict],
    utterances: list[dict],
    client_tool_calls: list[dict] | None,
) -> None:
    """Overlay client-tool activity onto the flattened rows (in place).

    `flat_turns` and `utterances` are parallel lists (same order/filtering).
    Calls are attributed by `after_utterance_id` (stamped by
    captures.reader._attribute_calls_to_turns): the owning turn is the one
    during which the call was dispatched.

    Per owning turn:
      - attach `tool_calls[]` (compact per-call rows);
      - `kind: "tool_invocation"` when the turn produced no spoken output
        (the model called a tool instead of speaking).

    For the turn *after* an invocation with resolved calls:
      - when its commit-anchored `tts.ttfb_ms` window contains the tool
        resolution (commit < resolved <= first-audio), the raw value is the
        tool round-trip plus the model's post-tool response — re-anchor it at
        the tool-result arrival (`ttfb_anchor: "tool_result"`, raw preserved
        as `ttfb_pre_tool_ms`) and recompute the critical path. Without a
        first-audio wallclock, conservatively subtract the round-trip
        (`ttfb_anchor: "tool_result_estimated"`).
      - `kind: "tool_followup"` + `tool_context` for either anchor, and also
        for the speechless echo leg some realtime providers emit after a
        fire-and-forget call (duplicated user item, no output).
    """
    if not client_tool_calls or not flat_turns:
        return
    by_utt: dict = {}
    for c in client_tool_calls:
        uid = c.get("after_utterance_id")
        if uid is not None:
            by_utt.setdefault(uid, []).append(c)
    if not by_utt:
        return

    for i, row in enumerate(flat_turns):
        own = by_utt.get(row.get("utt"))
        if not own:
            continue
        row["tool_calls"] = [_compact_tool_call(c) for c in own]
        tts = row.get("tts") or {}
        audio_ms = tts.get("audio_duration_ms")
        spoke = bool(row.get("agent_transcript")) or (
            isinstance(audio_ms, (int, float)) and audio_ms >= 100
        )
        if not spoke:
            row["kind"] = "tool_invocation"

        resolved = [
            c for c in own
            if c.get("status") in ("completed", "failed")
            and isinstance(c.get("resolved_at_ms"), (int, float))
        ]
        if not resolved or i + 1 >= len(flat_turns):
            continue
        resolved_at = max(float(c["resolved_at_ms"]) for c in resolved)
        roundtrips = [rt for rt in (tool_roundtrip_ms(c) for c in resolved) if rt is not None]
        max_rt = max(roundtrips) if roundtrips else None

        nxt = flat_turns[i + 1]
        nllm = nxt.get("llm") or {}
        ntts = nxt.get("tts") or {}
        raw_commit_fa = ntts.get("commit_to_first_audio_ms")
        raw_ttfb = ntts.get("ttfb_ms")
        use_commit_fa = (
            isinstance(raw_commit_fa, (int, float))
            and raw_commit_fa > 0
            and ntts.get("internal_ttfb_ms") is None
        )
        raw_span = raw_commit_fa if use_commit_fa else raw_ttfb
        wc = (utterances[i + 1].get("_wallclock_ts_ms") or {}) if i + 1 < len(utterances) else {}
        first_audio = wc.get("first_audio_out_ms")

        def _mark_followup(excluded_wait: float | None, nxt=nxt, row=row, max_rt=max_rt) -> None:
            nxt["kind"] = "tool_followup"
            ctx = {"invocation_utt": row.get("utt")}
            if max_rt is not None:
                ctx["tool_roundtrip_ms"] = round(max_rt, 1)
            if excluded_wait is not None:
                ctx["excluded_wait_ms"] = round(excluded_wait, 1)
            nxt["tool_context"] = ctx

        def _apply_span_adjustment(
            adjusted_span: float,
            excluded: float,
            anchor_label: str,
            use_commit_fa=use_commit_fa,
            raw_span=raw_span,
            ntts=ntts,
            nllm=nllm,
        ) -> None:
            if use_commit_fa:
                ntts["commit_to_first_audio_pre_tool_ms"] = raw_span
                ntts["commit_to_first_audio_ms"] = round(adjusted_span, 1)
                ntts["ttfb_anchor"] = anchor_label
                raw_llm_wait = nllm.get("llm_wait_ms")
                if isinstance(raw_llm_wait, (int, float)) and raw_llm_wait > excluded:
                    nllm["llm_wait_pre_tool_ms"] = raw_llm_wait
                    nllm["llm_wait_ms"] = round(float(raw_llm_wait) - excluded, 1)
            else:
                ntts["ttfb_pre_tool_ms"] = raw_span
                ntts["ttfb_ms"] = round(adjusted_span, 1)
                ntts["ttfb_anchor"] = anchor_label

        if isinstance(raw_span, (int, float)) and raw_span > 0:
            if isinstance(first_audio, (int, float)):
                commit = float(first_audio) - float(raw_span)
                # Re-anchor only when the response was actually gated on the
                # tool: the resolution falls inside the commit-to-first-audio
                # window. Resolved after first audio = the model spoke without
                # waiting; resolved before commit = a genuinely new turn — both
                # stay untouched.
                if commit < resolved_at <= float(first_audio):
                    adjusted = min(max(float(first_audio) - resolved_at, 0.0), float(raw_span))
                    excluded = float(raw_span) - adjusted
                    _apply_span_adjustment(adjusted, excluded, "tool_result")
                    _mark_followup(excluded)
                    nxt["critical_path"] = critical_path_latency(nxt)
            elif max_rt is not None and float(raw_span) > max_rt:
                # No wallclock to anchor on — subtract the round-trip. This
                # can only under-remove (the call may have been dispatched
                # before the commit), which errs against the pipeline, never
                # against the tool.
                adjusted = float(raw_span) - max_rt
                _apply_span_adjustment(adjusted, max_rt, "tool_result_estimated")
                _mark_followup(max_rt)
                nxt["critical_path"] = critical_path_latency(nxt)
        elif (
            row.get("kind") == "tool_invocation"
            and not nxt.get("agent_transcript")
            # A silent next turn with calls of its own is the next tool-call
            # leg (the user asked for something else), not this one's echo.
            and not by_utt.get(nxt.get("utt"))
        ):
            n_audio = ntts.get("audio_duration_ms")
            if not (isinstance(n_audio, (int, float)) and n_audio >= 100):
                _mark_followup(None)

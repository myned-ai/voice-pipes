import asyncio
import json
import logging
import os
import time
from datetime import UTC, datetime, timedelta
from pathlib import Path

import httpx

from ..captures.jsonl import parse_jsonl_line
from ..captures.reader import (
    _read_capture_artifacts,
    arrival_rtf,
    pick_client_speech_end,
    project_snapshot_metrics,
    project_throughput_metrics,
    resolve_speech_end_anchor,
)
from ..core import config
from ..core.config import (
    _CAPTURES_DIR_FALLBACK,
    _UTT_FILE_RE,
    ORCHESTRATOR_URL,
    _resolve_captures_dir,
    orch_client,
)
from ..issues import record_issue
from ..pricing import llm_cost_usd, monthly_cap_for, stt_cost_usd, tts_cost_usd
from ..report.analysis import _detect_anomalies, _extract_provider_identity, _flatten_turn, _iso_to_ts, resolve_grading
from . import rollup

_log = logging.getLogger("voice_pipes.analytics")

# Schema version — bump when the shape of a per-session rollup payload changes
# so old rows in rollups.db are transparently invalidated.
_ROLLUP_SCHEMA = 8  # v8: worker resource telemetry — peak_rss_bytes,
                    # container_peak_memory_bytes, avg_turn_cpu_ms in session rollups
                    # and window KPI aggregates.
                    # v7: per-utterance `chain_overhead_ms` is the
                    # post-processors' share only (0 with none configured)
                    # and the realtime pacer's wait is `pacer_wait_ms`.
                    # v6: a torn line in an event file no longer ends the
                    # read, so rollups cached from a truncated read are wrong
                    # and must be recomputed, not reused.
                    # v5: S2S latency anchoring — per-utterance
                    # `provider_endpointing_ms` / `first_text_ms` /
                    # `ttfb_anchor_source` / `e2e_anchor_source` and the
                    # window KPI `avg_provider_endpointing_ms`.
                    # v4: throughput observability — per-utterance
                    # `emit_rtf` / `source_rtf` / `chain_overhead_ms` /
                    # `post_budget_ratio_max` / `post_timeouts` / `arrival_rtf`
                    # and the per-session `throughput` rollup
                    # (avg/max emit_rtf, turns_over_realtime).
                    # v3: per-utterance `client_receive_to_play_ms`
                    # (audio_receive → playback_start timing reports).
                    # v2: per-utterance latencies sourced from the canonical
                    # metric snapshot under canonical key names
                    # (total_ms / ttfb_ms / ttft_ms / endpointing_ms).

# Bad-call auto-tag thresholds. Computed from the (already-parsed)
# per-session contribution, so tagging is free and pipeline-scoped.
_BADCALL_HIGH_LATENCY_MS = 3000   # a turn slower than this flags the call
_BADCALL_BARGE_N = 2              # ≥ this many barge-ins ⇒ "barge_storm"
_BADCALL_LOW_CSAT = 2            # csat ≤ this ⇒ "low_csat"


# ── Analytics response cache ─────────────────────────────────
# The /api/v1/analytics endpoint walks the entire captures dir per request,
# which the Dashboard hits twice on every load (current + prior period).
# Cache the fully-merged response keyed by (days, offset_days, signature).
# `signature` is a cheap (count, max_mtime) fingerprint over session dirs so
# the cache invalidates automatically when a session is added or its dir is
# touched.  TTL bounds staleness for live sessions whose events.jsonl is
# being appended to (file appends don't bump the parent dir mtime on
# Windows/Linux).
_ANALYTICS_CACHE_TTL_S = 30.0
_ANALYTICS_OVERLAY_TIMEOUT_S = 0.8
_analytics_cache: dict[tuple, tuple[float, dict]] = {}
_analytics_cache_lock = asyncio.Lock()

# 0a — per-key single-flight + bounded scan concurrency.  The response cache
# collapses *identical* requests; this registry lets *distinct* windows (the
# dashboard fires current + prior together) run concurrently instead of
# serializing behind one global lock, while the semaphore caps simultaneous
# disk walks so we don't thrash under load.
_analytics_inflight: dict[tuple, "asyncio.Future"] = {}
_analytics_scan_sem = asyncio.Semaphore(int(os.getenv("ANALYTICS_MAX_CONCURRENT_SCANS", "3")))

def _captures_signature() -> tuple[int, float]:
    """Fingerprint config.CAPTURES_DIR as (session_dir_count, max_mtime).
    Walks the directory without opening any files."""
    if config.CAPTURES_DIR == _CAPTURES_DIR_FALLBACK:
        _resolve_captures_dir()

    cleared_mt = rollup.cleared_mtime()
    if not config.CAPTURES_DIR.exists():
        return (0, cleared_mt)
    count = 0
    max_mt = cleared_mt
    try:
        for entry in config.CAPTURES_DIR.iterdir():
            if not entry.is_dir():
                continue
            count += 1
            try:
                mt = entry.stat().st_mtime
                if mt > max_mt:
                    max_mt = mt
            except OSError:
                pass
    except OSError:
        return (0, cleared_mt)
    return (count, max_mt)



def _persist_analytics_issues(issue_inputs: dict, spend_by_provider: dict) -> None:
    """Synchronous helper: write the scan-derived issues to SQLite.
    Runs as a background task of the analytics request, after the response
    is sent.  Failures are non-fatal."""
    try:
        for fs in issue_inputs.get("failed_sessions", []):
            record_issue(
                kind="failed_session",
                severity="error",
                dedup_key=f"failed_session:{fs['session_id']}",
                ts=_iso_to_ts(fs.get("timestamp")),
                session_id=fs["session_id"],
                pipeline_name=fs.get("pipeline_name"),
                summary=(
                    f"Session failed ({fs.get('reason')}): "
                    f"{fs.get('errors_total', 0)} errors across {fs.get('turn_count', 0)} turns"
                ),
                payload={
                    "errors_by_component": fs.get("errors_by_component"),
                    "last_error": fs.get("last_error"),
                    "failed_turn_count": fs.get("failed_turn_count"),
                },
            )
        for ch in issue_inputs.get("provider_changes", []):
            record_issue(
                kind="provider_change",
                severity="info",
                dedup_key=f"provider_change:{ch['session_id']}:{ch['field']}",
                ts=_iso_to_ts(ch.get("timestamp")),
                session_id=ch.get("session_id"),
                pipeline_name=None,
                summary=f"{ch['field']}: {ch['from_value']} → {ch['to_value']}",
                payload=ch,
            )
        for ans in issue_inputs.get("anomaly_sessions", []):
            if ans.get("anomalies_total", 0) >= 5:
                record_issue(
                    kind="anomaly_session",
                    severity="warn",
                    dedup_key=f"anomaly_session:{ans['session_id']}",
                    ts=_iso_to_ts(ans.get("timestamp")),
                    session_id=ans["session_id"],
                    pipeline_name=ans.get("pipeline_name"),
                    summary=(
                        f"{ans['anomalies_total']} anomalies: "
                        + ", ".join(f"{k} ({n})" for k, n in sorted(
                            ans.get("kind_counts", {}).items(),
                            key=lambda kv: kv[1], reverse=True,
                        )[:3])
                    ),
                    payload=ans.get("kind_counts"),
                )
        month_key = datetime.now(UTC).strftime("%Y-%m")
        for prov, vals in (spend_by_provider or {}).items():
            cap_pct = vals.get("cap_pct")
            if cap_pct is not None and cap_pct >= 100:
                record_issue(
                    kind="quota_breach",
                    severity="error",
                    dedup_key=f"quota_breach:{prov}:{month_key}",
                    summary=(
                        f"{prov} crossed monthly cap: "
                        f"${vals.get('total', 0):.2f} of ${vals.get('monthly_cap_usd', 0):.2f} "
                        f"({cap_pct:.1f}%)"
                    ),
                    payload={"provider": prov, **vals},
                )
        # Bad-call auto-tags into the feed (one issue per session,
        # deduped by session so re-scans don't pile up).
        for bc in issue_inputs.get("bad_calls", []):
            tags = bc.get("tags") or []
            if not tags:
                continue
            record_issue(
                kind="bad_call",
                severity="warn",
                dedup_key=f"bad_call:{bc['session_id']}",
                ts=_iso_to_ts(bc.get("timestamp")),
                session_id=bc["session_id"],
                pipeline_name=bc.get("pipeline_name"),
                summary="flagged: " + ", ".join(tags),
                payload={"tags": tags},
            )
    except Exception:  # noqa: BLE001 - logged below; persistence is best effort
        # The store's own failures are handled by record_issue; anything
        # reaching here is a defect, and the analytics response is already out.
        _log.warning("analytics issues could not be persisted", exc_info=True)


def _session_anomaly_contribution(session_id, meta, pipeline_name, session_ts) -> dict:
    """Run the anomaly rubric for one session and return its contribution to the
    window rollup: ``{"total": n, "by_kind": {...}, "session": row|None}``.

    Isolated so the (expensive) artifact walk it performs is captured into the
    durable per-session rollup (see rollup.py) and runs once per session change.
    """
    arts = _read_capture_artifacts(session_id, events_offset=0, events_limit=5000)
    sess_utts = arts.get("utterances") or []
    if not sess_utts:
        return {"total": 0, "by_kind": {}, "session": None}
    providers = _extract_provider_identity(sess_utts, meta)
    first_utt_id = sess_utts[0].get("utterance_id")
    flat = [_flatten_turn(u, meta.get("started_at"), first_utt_id, providers) for u in sess_utts]
    # Through the shared resolver, so the dashboard counts anomalies against
    # the same targets the session's own report does -- the operator's, and the
    # budgets its components declared, which this used to leave out.
    anoms = _detect_anomalies(flat, resolve_grading(providers, meta)["targets"])
    by_kind: dict[str, int] = {}
    for a in anoms:
        k = a.get("kind") or "unknown"
        by_kind[k] = by_kind.get(k, 0) + 1
    session = None
    if anoms:
        session = {
            "session_id": session_id,
            "pipeline_name": pipeline_name,
            "timestamp": session_ts,
            "anomalies_total": len(anoms),
            "kind_counts": by_kind,
        }
    return {"total": len(anoms), "by_kind": by_kind, "session": session}


def _compute_session_rollup(session_dir: Path, meta: dict, *, lite: bool = False) -> dict:
    """Parse ONE session's files into its reduced window contribution.

    This is the cacheable unit behind the durable rollup store: it does all the
    per-session I/O (events.jsonl + turn files + the anomaly artifact walk) and
    returns a JSON-serialisable dict the window merge replays into the global
    accumulators.  Cost calls are captured as an ordered ``cost_events`` list so
    the merge can replay them through the window's ``_add_cost`` in the exact
    original order — keeping spend rounding byte-identical.
    """
    session_id = meta["id"]
    pipeline_name = meta.get("pipeline_name", "")
    stt_prov = meta.get("stt_provider", "")
    tts_prov = meta.get("tts_provider", "")
    session_ts = meta.get("start_time", "")

    utts: list[dict] = []
    s_total_latencies: list[float] = []
    cost_events: list[list] = []

    llm_timings: list[dict] = []
    stt_usage: dict = {}
    stt_by_model: dict[tuple[str, str], dict[str, float]] = {}
    tts_usage_chars: int = 0
    tts_usage_audio_s: float = 0.0
    tts_voice: str | None = None
    tts_by_voice: dict[tuple[str, str], dict[str, float]] = {}
    client_timings: dict[int, list[dict]] = {}
    barge_in_rtts: dict[int, float] = {}
    snapshots: dict[int, dict] = {}  # utterance_id -> canonical TurnMetricsSnapshot
    session_errors: dict[str, int] = {"stt": 0, "llm": 0, "tts": 0, "other": 0}
    utt_error_components: dict[int, set[str]] = {}
    last_error_msg: str | None = None
    events_path = session_dir / "events.jsonl"
    if events_path.exists():
        try:
            with open(events_path, encoding="utf-8") as f:
                for line in f:
                    if not line.strip():
                        continue
                    evt = parse_jsonl_line(line)
                    if evt is None:
                        continue
                    etype = evt.get("event_type") or evt.get("type") or ""
                    if etype.endswith("_error") or etype == "error":
                        if etype == "error":
                            comp = ((evt.get("payload") or {}).get("component") or "other").lower()
                        else:
                            comp = etype[: -len("_error")].lower()
                        bucket = comp if comp in session_errors else "other"
                        session_errors[bucket] += 1
                        payload = evt.get("payload") or {}
                        uid = payload.get("utterance_id")
                        if uid is not None:
                            utt_error_components.setdefault(uid, set()).add(bucket)
                        msg = payload.get("message") or payload.get("error") or payload.get("reason")
                        if isinstance(msg, str) and msg:
                            last_error_msg = msg
                        continue
                    if etype == "llm_timing":
                        llm_timings.append(evt.get("payload", {}))
                    elif etype == "stt_usage":
                        stt_usage = evt.get("payload", {})
                        sp = (stt_usage.get("provider") or stt_prov or "unknown")
                        sm = (stt_usage.get("speech_model") or stt_usage.get("model") or "")
                        slot = stt_by_model.setdefault((sp, sm), {"audio_s": 0.0, "session_s": 0.0})
                        slot["audio_s"] += float(stt_usage.get("audio_duration_seconds") or 0.0)
                        slot["session_s"] += float(stt_usage.get("session_duration_seconds") or 0.0)
                    elif etype == "tts_usage":
                        p = evt.get("payload", {})
                        chars = int(p.get("chars", 0) or 0)
                        audio_s = float(p.get("audio_duration_s", 0.0) or 0.0)
                        tts_usage_chars += chars
                        tts_usage_audio_s += audio_s
                        if p.get("voice"):
                            tts_voice = p["voice"]
                        tp = p.get("provider") or tts_prov or "unknown"
                        tv = p.get("voice") or ""
                        slot = tts_by_voice.setdefault((tp, tv), {"chars": 0.0, "audio_s": 0.0})
                        slot["chars"] += chars
                        slot["audio_s"] += audio_s
                    elif etype == "client_timing":
                        p = evt.get("payload", {})
                        uid = p.get("utterance_id")
                        if uid is not None:
                            client_timings.setdefault(uid, []).append({
                                "event": p.get("event"),
                                "client_timestamp_ms": p.get("client_timestamp_ms"),
                            })
                    elif etype == "barge_in_rtt":
                        p = evt.get("payload", {})
                        uid = p.get("utterance_id")
                        if uid is not None:
                            barge_in_rtts[uid] = p.get("rtt_ms")
        except Exception:  # noqa: BLE001 - logged below
            # A malformed capture contributes what was read before the bad
            # row; it must not drop the whole window.
            _log.warning("events of session %s were only partly read", session_dir.name, exc_info=True)

    # Canonical per-turn snapshots live in pipeline_events.jsonl as
    # `turn_metrics_ready` events. These are the single source of truth for
    # per-utterance latencies (total/stt/llm/tts); the flat turn_resolved.json
    # carries only lifecycle fields now.
    pipeline_events_path = session_dir / "pipeline_events.jsonl"
    if pipeline_events_path.exists():
        try:
            with open(pipeline_events_path, encoding="utf-8") as f:
                for line in f:
                    if not line.strip():
                        continue
                    pe = parse_jsonl_line(line)
                    if pe is None:
                        continue
                    if pe.get("kind") == "turn_metrics_ready":
                        uid = pe.get("utterance_id")
                        metrics = pe.get("metrics")
                        if uid is not None and metrics is not None:
                            snapshots[uid] = metrics
        except (OSError, ValueError):
            # Unreadable file: the session counts without its snapshots.
            pass

    utt_idx = 0
    session_turn_count = 0
    session_failed_turn_count = 0
    emit_rtf_vals: list[float] = []
    for fp in sorted(session_dir.iterdir()):
        m = _UTT_FILE_RE.match(fp.name)
        if not m or m.group("kind") != "turn_resolved" or m.group("ext") != "json":
            continue
        try:
            with open(fp, encoding="utf-8") as fh:
                summary = json.load(fh)
        except (OSError, ValueError):
            continue

        utt_id_key = summary.get("utterance_id")
        # Canonical per-turn latencies from the metric snapshot (single source
        # of truth); the flat summary contributes lifecycle fields only.
        proj = project_snapshot_metrics(snapshots.get(utt_id_key))
        total_lat = proj["total_ms"]

        # Throughput: the `output` and
        # `audio_post:*` components of the same snapshot, plus the client's
        # playback frame (`NNN_playback.json`, sibling of this turn file) for
        # `arrival_rtf`. All None on captures that predate them.
        thr = project_throughput_metrics(snapshots.get(utt_id_key))
        out_c = thr["output"] or {}
        posts = thr["audio_post"]
        post_ratios = [
            r for r in ((p or {}).get("budget_ratio") for p in posts.values())
            if isinstance(r, (int, float))
        ]
        post_budget_ratio_max = max(post_ratios) if post_ratios else None
        post_timeouts = (
            sum(int((p or {}).get("timeouts") or 0) for p in posts.values()) if posts else None
        )
        playback_path = session_dir / f"{m.group('utt')}_playback.json"
        utt_arrival_rtf = None
        if playback_path.exists():
            try:
                with open(playback_path, encoding="utf-8") as fh:
                    utt_arrival_rtf = arrival_rtf(json.load(fh))
            except (OSError, ValueError):
                utt_arrival_rtf = None
        emit_rtf = out_c.get("emit_rtf")
        if isinstance(emit_rtf, (int, float)):
            emit_rtf_vals.append(float(emit_rtf))
        if total_lat is not None:
            s_total_latencies.append(total_lat)
        session_turn_count += 1

        llm = llm_timings[utt_idx] if utt_idx < len(llm_timings) else {}
        utt_idx += 1

        usage = llm.get("usage") or {}
        p_tok = llm.get("prompt_tokens") or usage.get("prompt_tokens")
        o_tok = llm.get("output_tokens") or usage.get("completion_tokens") or usage.get("output_tokens")

        ct_events = client_timings.get(utt_id_key, [])

        def _client_ts(event_name: str):
            ev = next((e for e in ct_events if e.get("event") == event_name), None)  # noqa: B023 - called only within this iteration
            return ev.get("client_timestamp_ms") if ev else None

        playback_start_ms = _client_ts("playback_start")
        # Not `_client_ts`, which takes the first match: a client reports one
        # `speech_end` per pause, and the first is where the speaker hesitated,
        # not where they stopped. Same rule as the session report, so the
        # dashboard and the report cannot disagree about one turn.
        speech_end_ms = pick_client_speech_end(
            [e.get("client_timestamp_ms") for e in ct_events if e.get("event") == "speech_end"],
            _client_ts("audio_receive"),
        )
        if (
            playback_start_ms is not None
            and speech_end_ms is not None
            and playback_start_ms >= speech_end_ms
        ):
            client_ttfa_ms = playback_start_ms - speech_end_ms
        else:
            client_ttfa_ms = None
        # Jitter buffer + scheduling on the client: first audio byte received →
        # first chunk scheduled. Same events the Playground's live "Buffer" pill
        # is derived from, minus the AudioContext lead (`startTime − currentTime`
        # + output latency), so the two usually agree within ~30 ms.
        audio_receive_ms = _client_ts("audio_receive")
        if (
            playback_start_ms is not None
            and audio_receive_ms is not None
            and playback_start_ms >= audio_receive_ms
        ):
            client_receive_to_play_ms = playback_start_ms - audio_receive_ms
        else:
            client_receive_to_play_ms = None
        audio_underruns = sum(1 for e in ct_events if e.get("event") == "underrun")

        # S2S anchoring. `e2e_anchor_source`
        # is what an E2E for this turn would be anchored on, by the reader's
        # precedence (client `speech_end` › vp VAD › provider anchor); None
        # when the turn has no anchor at all (cascade turns without a client
        # stamp — the analytics scan does not replay the stt_timing fallback).
        _, e2e_anchor_source = resolve_speech_end_anchor({
            "metric_snapshot": snapshots.get(utt_id_key),
            "_wallclock_ts_ms": (
                {"client_speech_end_ms": speech_end_ms}
                if isinstance(speech_end_ms, (int, float)) else {}
            ),
        })

        err_comps = sorted(utt_error_components.get(utt_id_key, set()))
        had_error = bool(err_comps) or total_lat is None
        if had_error:
            session_failed_turn_count += 1

        utts.append({
            "session_id": session_id,
            "utterance_id": utt_id_key,
            "timestamp": session_ts,
            "total_ms": total_lat,
            "ttfb_ms": proj["ttfb_ms"],
            "ttft_ms": proj["ttft_ms"],
            "endpointing_ms": proj["endpointing_ms"],
            # Realtime S2S anchoring — all None on cascade turns and on
            # captures that predate the keys. `provider_endpointing_ms` is
            # the S2S counterpart of `endpointing_ms` (kept apart: different
            # mechanism, separate Dashboard KPI).
            "provider_endpointing_ms": proj["provider_endpointing_ms"],
            "first_text_ms": proj["first_text_ms"],
            "ttfb_anchor_source": proj["ttfb_anchor_source"],
            "e2e_anchor_source": e2e_anchor_source,
            "stt_provider": summary.get("stt_provider", stt_prov),
            "tts_provider": summary.get("tts_provider", tts_prov),
            "pipeline_name": pipeline_name,
            "tokens_per_sec": llm.get("tokens_per_sec") or (
                round(o_tok / (llm.get("elapsed_ms") / 1000), 1)
                if o_tok and llm.get("elapsed_ms")
                else None
            ),
            "first_token_ms": llm.get("first_token_ms"),
            "prompt_tokens": p_tok,
            "output_tokens": o_tok,
            "generate_ms": llm.get("generate_ms") or llm.get("elapsed_ms"),
            "model": llm.get("model"),
            "stt_audio_duration_s": stt_usage.get("audio_duration_seconds"),
            "stt_session_duration_s": stt_usage.get("session_duration_seconds"),
            "stt_speech_model": stt_usage.get("speech_model"),
            "tts_chars": tts_usage_chars or None,
            "tts_audio_duration_s": tts_usage_audio_s or None,
            "tts_voice": tts_voice,
            "tts_usage_by_voice": [
                {"provider": p, "voice": v, "chars": int(d["chars"]), "audio_s": round(d["audio_s"], 3)}
                for (p, v), d in tts_by_voice.items()
            ] or None,
            "stt_usage_by_model": [
                {"provider": p, "model": m, "audio_s": round(d["audio_s"], 3), "session_s": round(d["session_s"], 3)}
                for (p, m), d in stt_by_model.items()
            ] or None,
            "client_ttfa_ms": client_ttfa_ms,
            "client_receive_to_play_ms": client_receive_to_play_ms,
            # Streaming-phase throughput (rtf = wall ÷ audio; > 1.0 = slower
            # than real time). None when the capture has no `output` component.
            "emit_rtf": emit_rtf,
            "source_rtf": out_c.get("source_rtf"),
            "chain_overhead_ms": out_c.get("chain_overhead_ms"),
            "pacer_wait_ms": out_c.get("pacer_wait_ms"),
            "post_budget_ratio_max": post_budget_ratio_max,
            "post_timeouts": post_timeouts,
            "arrival_rtf": utt_arrival_rtf,
            "audio_underruns": audio_underruns or None,
            "barge_in_rtt_ms": barge_in_rtts.get(utt_id_key),
            "had_error": had_error,
            "error_components": err_comps,
            "llm_cost_usd": llm_cost_usd(llm.get("model"), p_tok, o_tok),
        })
        cost_events.append([llm.get("model") or "", "llm", llm_cost_usd(llm.get("model"), p_tok, o_tok)])

    for (sp, _sm), s_d in stt_by_model.items():
        cost_events.append([sp, "stt", stt_cost_usd(sp, s_d.get("audio_s"))])
    for (tp, _tv), t_d in tts_by_voice.items():
        cost_events.append([tp, "tts", tts_cost_usd(tp, int(t_d.get("chars") or 0))])

    session_total_errors = sum(session_errors.values())
    is_failed = session_total_errors > 0 or session_turn_count == 0
    failed_session = None
    if is_failed:
        failed_session = {
            "session_id": session_id,
            "timestamp": session_ts,
            "pipeline_name": pipeline_name,
            "stt_provider": stt_prov,
            "tts_provider": tts_prov,
            "errors_by_component": dict(session_errors),
            "errors_total": session_total_errors,
            "turn_count": session_turn_count,
            "failed_turn_count": session_failed_turn_count,
            "last_error": last_error_msg,
            "reason": (
                "no_turns" if session_turn_count == 0
                else "errors_recorded"
            ),
        }

    if lite:
        anomaly = {"total": 0, "by_kind": {}, "session": None}
    else:
        try:
            anomaly = _session_anomaly_contribution(session_id, meta, pipeline_name, session_ts)
        except Exception:  # noqa: BLE001 - logged; one session must not fail the window
            _log.warning("anomalies of session %s could not be computed", session_id, exc_info=True)
            anomaly = {"total": 0, "by_kind": {}, "session": None}

    # Session-level output throughput rollup. `turns_over_realtime`
    # counts turns whose streaming phase ran behind playback (emit_rtf > 1.0).
    throughput = {
        "avg_emit_rtf": (
            round(sum(emit_rtf_vals) / len(emit_rtf_vals), 3) if emit_rtf_vals else None
        ),
        "max_emit_rtf": round(max(emit_rtf_vals), 3) if emit_rtf_vals else None,
        "turns_over_realtime": sum(1 for v in emit_rtf_vals if v > 1.0),
    }

    # Worker resource efficiency (CPU & RAM) from session metadata
    resources = {
        "peak_rss_bytes": meta.get("peak_rss_bytes"),
        "container_peak_memory_bytes": meta.get("container_peak_memory_bytes"),
        "avg_turn_cpu_ms": meta.get("avg_turn_cpu_ms"),
    }

    return {
        "utterances": utts,
        "total_latencies": s_total_latencies,
        "cost_events": cost_events,
        "session_errors": session_errors,
        "failed_turn_count": session_failed_turn_count,
        "failed_session": failed_session,
        "anomaly": anomaly,
        "throughput": throughput,
        "resources": resources,
    }


def _compute_analytics_window(
    days: int,
    offset_days: int,
    pipeline: str | None = None,
    *,
    lite: bool = False,
) -> dict:
    """Sync helper: walk captures and build the analytics payload for the
    given window.  Runs in a worker thread via asyncio.to_thread so the
    event loop stays free for concurrent requests.

    ``pipeline`` (1b) restricts the scan to one pipeline_name: non-matching
    sessions are skipped during collection — *before* any rollup/parse — so a
    single-pipeline query never touches another pipeline's session data.
    ``None`` = all pipelines (unchanged behavior).

    ``lite`` skips the per-session anomaly artifact walk for the simplified
    dashboard mode.

    Returns ``{"response": {...}, "issue_inputs": {...}}`` where
    ``response`` is the payload to merge with the orchestrator overlay and
    return to the caller, and ``issue_inputs`` carries the raw failed /
    provider-change / anomaly lists for background SQLite persistence.
    """
    _t0 = time.perf_counter()
    if not config.CAPTURES_DIR.exists():
        return {
            "response": {"kpi": {}, "utterances": [], "provider_changes": [], "available_pipelines": []},
            "issue_inputs": {
                "failed_sessions": [],
                "provider_changes": [],
                "anomaly_sessions": [],
            },
        }

    now = datetime.now(UTC)
    start_of_today = now.replace(hour=0, minute=0, second=0, microsecond=0)
    cutoff = (start_of_today - timedelta(days=days - 1 + offset_days)).timestamp()
    # Upper bound: only relevant when offset > 0 (prior-period query).
    cutoff_end: float | None = None
    if offset_days > 0:
        cutoff_end = (start_of_today - timedelta(days=offset_days - 1)).timestamp()

    cleared = rollup.get_cleared_state()
    global_cleared_at: float = cleared.get("global_cleared_at", 0.0)
    cleared_pipelines: dict[str, float] = cleared.get("pipelines", {})
    cleared_sessions: set[str] = cleared.get("sessions", set())
    if global_cleared_at > cutoff:
        cutoff = global_cleared_at

    # 1b — optional pipeline partition.  Comma-separated supports the FE's
    # multi-select; ``None`` = all pipelines (unchanged behavior).
    pipeline_set = {p for p in (pipeline.split(",") if pipeline else []) if p} or None

    # Collect session metadata sorted by time.  ``all_pipelines`` records every
    # pipeline present in the window *regardless of the filter*, so the FE's
    # pipeline picker stays fully populated even when a filter is applied.
    all_pipelines: set[str] = set()
    session_metas: list[dict] = []
    for entry in config.CAPTURES_DIR.iterdir():
        if not entry.is_dir():
            continue
        meta_path = entry / "metadata.json"
        if not meta_path.exists():
            continue
        try:
            with open(meta_path, encoding="utf-8") as f:
                meta = json.load(f)
            sid = str(meta.get("id") or meta.get("session_id") or entry.name)
            meta["id"] = sid
            if sid in cleared_sessions or entry.name in cleared_sessions:
                continue
            # Parse start_time first so the window bound applies to both the
            # picker options and the partition filter.
            st = meta.get("start_time", "")
            if not st:
                continue
            ts = datetime.fromisoformat(st.replace("Z", "+00:00")).timestamp()
            if ts < cutoff or (global_cleared_at > 0 and ts <= global_cleared_at):
                continue
            if cutoff_end is not None and ts >= cutoff_end:
                continue
            pname = meta.get("pipeline_name", "")
            pipe_cleared_at = cleared_pipelines.get(pname, 0.0)
            if pipe_cleared_at > 0 and ts <= pipe_cleared_at:
                continue
            meta["_ts"] = ts
            all_pipelines.add(pname)
            # 1b — drop non-matching sessions before any rollup/parse work, but
            # only after recording the option list above.
            if pipeline_set is not None and pname not in pipeline_set:
                continue
            meta["_dir"] = str(entry)
            session_metas.append(meta)
        except (OSError, ValueError, TypeError, AttributeError):
            # Unreadable metadata or an unparseable start_time: the session
            # cannot be placed in any window.
            continue

    session_metas.sort(key=lambda s: s.get("_ts", 0))

    # Detect provider/pipeline changes between consecutive sessions
    provider_changes: list[dict] = []
    tracked_fields = ["stt_provider", "tts_provider", "pipeline_name"]
    for i in range(1, len(session_metas)):
        prev, curr = session_metas[i - 1], session_metas[i]
        for field in tracked_fields:
            old_val = prev.get(field)
            new_val = curr.get(field)
            if old_val and new_val and old_val != new_val:
                provider_changes.append({
                    "timestamp": curr.get("start_time"),
                    "field": field,
                    "from_value": old_val,
                    "to_value": new_val,
                    "session_id": curr["id"],
                })

    # Collect per-utterance data + llm_timing from events
    utterances: list[dict] = []
    total_latencies: list[float] = []
    # Sessions that produced no successful turn or recorded any error event.
    failed_sessions: list[dict] = []
    # Aggregate error-event counts per component across the window.
    errors_by_component: dict[str, int] = {"stt": 0, "llm": 0, "tts": 0, "other": 0}
    failed_turns_total = 0
    # Spend rollup keyed by provider root (e.g. "openai", "elevenlabs").  Each
    # bucket carries totals split by component so the FE can render breakdowns.
    spend_by_provider: dict[str, dict[str, float]] = {}

    # P3 #17 — Anomaly summary across the window.  We re-run the same
    # `_detect_anomalies` rubric the per-session detail uses, then roll counts
    # up by kind and keep the top N sessions for the UI's anomaly feed.
    anomalies_total = 0
    anomalies_by_kind: dict[str, int] = {}
    anomaly_sessions: list[dict] = []
    # 0b/0d — anomaly memo hit/miss counters for the optional timing log.
    _memo_hits = 0
    _memo_miss = 0
    # Business outcomes + CSAT, read from session metadata (already
    # loaded during collection, so this costs nothing extra and is scoped to the
    # pipeline filter via session_metas).
    outcome_counts = {"contained": 0, "escalated": 0, "abandoned": 0}
    csat_sum = 0.0
    csat_n = 0
    # Bad-call auto-tags: sessions flagged by symptom for the feed.
    flagged_sessions: list[dict] = []
    # Throughput observability — window-wide emit_rtf distribution + the count
    # of turns that ran behind real time (summed from the per-session rollups).
    emit_rtf_window: list[float] = []
    turns_over_realtime_total = 0
    # S2S provider endpointing over the window: how late the provider noticed
    # the end of speech, per S2S turn.
    provider_endpt_window: list[float] = []
    # Resource metrics distributions over the window
    peak_rss_window: list[int] = []
    container_peak_window: list[int] = []
    avg_turn_cpu_window: list[float] = []

    def _add_cost(provider: str | None, component: str, cost: float | None) -> None:
        """Bump ``spend_by_provider`` for the given (provider, component) pair.
        ``None`` cost means "untracked" and is silently ignored."""
        if cost is None or not provider:
            return
        root = provider.split("/", 1)[0].lower()
        slot = spend_by_provider.setdefault(root, {"llm": 0.0, "stt": 0.0, "tts": 0.0, "total": 0.0})
        slot[component] = round(slot.get(component, 0.0) + cost, 6)
        slot["total"] = round(slot["total"] + cost, 6)

    for meta in session_metas:
        session_dir = Path(meta["_dir"])
        session_id = meta["id"]
        ev_path = session_dir / "events.jsonl"
        ev_mtime = ev_path.stat().st_mtime if ev_path.exists() else 0.0
        try:
            dir_mtime = session_dir.stat().st_mtime
        except OSError:
            dir_mtime = 0.0
        # Reuse the durable rollup when the session's files are unchanged;
        # otherwise parse once and store.  Either path yields the same
        # reduced per-session facts in ``contrib``.
        contrib = rollup.get_fresh(
            session_id, ev_mtime, dir_mtime, _ROLLUP_SCHEMA, allow_lite=lite,
        )
        if contrib is not None:
            _memo_hits += 1
        else:
            contrib = (
                _compute_session_rollup(session_dir, meta, lite=True)
                if lite
                else _compute_session_rollup(session_dir, meta)
            )
            schema_to_store = -_ROLLUP_SCHEMA if lite else _ROLLUP_SCHEMA
            rollup.put(session_id, meta.get("pipeline_name", ""), meta.get("_ts", 0.0),
                       ev_mtime, dir_mtime, schema_to_store, contrib)
            _memo_miss += 1

        # Merge this session's contribution.  session_metas is start-time
        # sorted and cost events replay in captured order, so the global spend
        # rounding stays byte-identical to the pre-rollup single-pass version.
        utterances.extend(contrib["utterances"])
        total_latencies.extend(contrib["total_latencies"])
        for _ev in contrib["cost_events"]:
            _add_cost(_ev[0], _ev[1], _ev[2])
        for _k, _v in contrib["session_errors"].items():
            errors_by_component[_k] += _v
        failed_turns_total += contrib["failed_turn_count"]
        if contrib["failed_session"]:
            failed_sessions.append(contrib["failed_session"])
        _anom = contrib["anomaly"]
        anomalies_total += _anom["total"]
        for _kind, _n in _anom["by_kind"].items():
            anomalies_by_kind[_kind] = anomalies_by_kind.get(_kind, 0) + _n
        if _anom["session"]:
            anomaly_sessions.append(_anom["session"])
        _thr = contrib.get("throughput") or {}
        turns_over_realtime_total += int(_thr.get("turns_over_realtime") or 0)
        emit_rtf_window.extend(
            float(u["emit_rtf"]) for u in contrib["utterances"]
            if isinstance(u.get("emit_rtf"), (int, float))
        )
        provider_endpt_window.extend(
            float(u["provider_endpointing_ms"]) for u in contrib["utterances"]
            if isinstance(u.get("provider_endpointing_ms"), (int, float))
        )

        _res = contrib.get("resources") or {}
        if isinstance(_res.get("peak_rss_bytes"), int) and _res["peak_rss_bytes"] > 0:
            peak_rss_window.append(_res["peak_rss_bytes"])
        if isinstance(_res.get("container_peak_memory_bytes"), int) and _res["container_peak_memory_bytes"] > 0:
            container_peak_window.append(_res["container_peak_memory_bytes"])
        if isinstance(_res.get("avg_turn_cpu_ms"), (int, float)) and _res["avg_turn_cpu_ms"] >= 0:
            avg_turn_cpu_window.append(float(_res["avg_turn_cpu_ms"]))

        # Business outcome + CSAT from this session's metadata.
        oc = meta.get("outcome")
        if oc in outcome_counts:
            outcome_counts[oc] += 1
        csat_v = meta.get("csat")
        if isinstance(csat_v, (int, float)) and not isinstance(csat_v, bool):
            csat_sum += float(csat_v)
            csat_n += 1

        # Bad-call auto-tags from the session's own turns.
        s_utts = contrib["utterances"]
        tags: list[str] = []
        if any((u.get("total_ms") or 0) > _BADCALL_HIGH_LATENCY_MS for u in s_utts):
            tags.append("high_latency")
        if any(u.get("audio_underruns") for u in s_utts):
            tags.append("underrun")
        if sum(1 for u in s_utts if u.get("barge_in_rtt_ms") is not None) >= _BADCALL_BARGE_N:
            tags.append("barge_storm")
        if contrib["failed_session"]:
            tags.append("errors")
        if isinstance(csat_v, (int, float)) and not isinstance(csat_v, bool) and csat_v <= _BADCALL_LOW_CSAT:
            tags.append("low_csat")
        if tags:
            flagged_sessions.append({
                "session_id": session_id,
                "pipeline_name": meta.get("pipeline_name", ""),
                "timestamp": meta.get("start_time", ""),
                "tags": tags,
            })

    outcome_known = sum(outcome_counts.values())

    # Compute KPIs
    total_latencies_clean = [x for x in total_latencies if x is not None]
    sorted_lat = sorted(total_latencies_clean) if total_latencies_clean else []
    p95_idx = int(len(sorted_lat) * 0.95) if sorted_lat else 0

    tps_values = [u["tokens_per_sec"] for u in utterances if u.get("tokens_per_sec") is not None]

    emit_sorted = sorted(emit_rtf_window)
    emit_p90_idx = min(len(emit_sorted) - 1, int(len(emit_sorted) * 0.9)) if emit_sorted else 0

    rss_sorted = sorted(peak_rss_window)
    rss_p95_idx = min(len(rss_sorted) - 1, int(len(rss_sorted) * 0.95)) if rss_sorted else 0

    # Error rate is per-turn: failed turns / (successful turns + failed turns).
    # `len(utterances)` already counts every turn we saw a `turn_resolved`
    # file for, regardless of success.
    total_observed_turns = len(utterances)
    error_rate = (
        round(failed_turns_total / total_observed_turns * 1000) / 10
        if total_observed_turns > 0 else None
    )

    kpi = {
        "total_sessions": len(session_metas),
        "total_utterances": len(utterances),
        "avg_latency_ms": round(sum(total_latencies_clean) / len(total_latencies_clean), 1) if total_latencies_clean else None,
        "p95_latency_ms": round(sorted_lat[p95_idx], 1) if sorted_lat else None,
        "avg_tokens_per_sec": round(sum(tps_values) / len(tps_values), 1) if tps_values else None,
        "failed_sessions": len(failed_sessions),
        "failed_turns": failed_turns_total,
        "error_rate_pct": error_rate,
        "errors_by_component": errors_by_component,
        # Window-scoped USD spend.  ``spend_by_provider`` is keyed by the
        # vendor root (openai, anthropic, …) and contains llm/stt/tts/total
        # subtotals.  ``spend_total_usd`` is the sum across providers.
        "spend_total_usd": round(sum(p.get("total", 0.0) for p in spend_by_provider.values()), 4),
        "spend_by_provider": {
            name: {
                **vals,
                "monthly_cap_usd": monthly_cap_for(name),
                "cap_pct": (
                    round(vals["total"] / monthly_cap_for(name) * 1000) / 10
                    if monthly_cap_for(name) and vals.get("total") is not None
                    else None
                ),
            }
            for name, vals in spend_by_provider.items()
        },
        # P3 #17 — Anomaly counts.  ``anomalies_total`` is the sum across all
        # sessions in the window; ``anomalies_by_kind`` breaks it down by the
        # rubric's `kind` field (e.g. ``llm.ttft_high``, ``stt.endpointing_high``).
        "anomalies_total": anomalies_total,
        "anomalies_by_kind": anomalies_by_kind,
        # Throughput observability — output realtime factor over the window
        # (rtf = wall ÷ audio; > 1.0 = the client starves). None / 0 when no
        # capture in the window carries the `output` component (old captures).
        "avg_emit_rtf": (round(sum(emit_sorted) / len(emit_sorted), 3) if emit_sorted else None),
        "p90_emit_rtf": (round(emit_sorted[emit_p90_idx], 3) if emit_sorted else None),
        "max_emit_rtf": (round(emit_sorted[-1], 3) if emit_sorted else None),
        "turns_over_realtime": turns_over_realtime_total,
        # S2S provider endpointing rollup:
        # mean `provider_endpointing_ms` over the window's S2S turns; None
        # when no capture carries the key (cascade-only windows, old captures).
        "avg_provider_endpointing_ms": (
            round(sum(provider_endpt_window) / len(provider_endpt_window), 1)
            if provider_endpt_window else None
        ),
        # Worker resource efficiency (CPU & RAM):
        # Average & p95 peak worker RSS bytes, maximum container peak memory bytes,
        # and mean turn CPU compute time (ms).
        "avg_peak_rss_bytes": (
            round(sum(peak_rss_window) / len(peak_rss_window))
            if peak_rss_window else None
        ),
        "p95_peak_rss_bytes": (
            rss_sorted[rss_p95_idx]
            if rss_sorted else None
        ),
        "max_container_peak_bytes": (
            max(container_peak_window)
            if container_peak_window else None
        ),
        "avg_turn_cpu_ms": (
            round(sum(avg_turn_cpu_window) / len(avg_turn_cpu_window), 1)
            if avg_turn_cpu_window else None
        ),
        # Business outcomes / CSAT (None when the captures carry no
        # outcome tags, so the FE cards hide gracefully).
        "contained": outcome_counts["contained"],
        "escalated": outcome_counts["escalated"],
        "abandoned": outcome_counts["abandoned"],
        "outcome_known": outcome_known,
        "containment_rate_pct": (round(outcome_counts["contained"] / outcome_known * 1000) / 10 if outcome_known else None),
        "escalation_rate_pct": (round(outcome_counts["escalated"] / outcome_known * 1000) / 10 if outcome_known else None),
        "avg_csat": (round(csat_sum / csat_n, 2) if csat_n else None),
        "csat_samples": csat_n,
    }

    # Orchestrator overlay + issue persistence happen in the async wrapper so
    # they don't block this scan body (which runs in a worker thread).

    # Top N sessions by anomaly count for the FE feed.
    anomaly_sessions.sort(key=lambda s: s.get("anomalies_total", 0), reverse=True)

    # Issue persistence inputs — the route hands them to
    # _persist_analytics_issues as a background task of the request.
    issue_inputs = {
        "failed_sessions": failed_sessions,
        "provider_changes": provider_changes,
        "anomaly_sessions": anomaly_sessions,
        "bad_calls": flagged_sessions,
    }

    if os.getenv("DEBUG_TIMING"):
        _log.info(
            "analytics scan days=%s offset=%s sessions=%d utterances=%d "
            "anomaly_memo(hit=%d miss=%d) elapsed_ms=%.0f",
            days, offset_days, len(session_metas), len(utterances),
            _memo_hits, _memo_miss, (time.perf_counter() - _t0) * 1000.0,
        )

    return {
        "response": {
            "kpi": kpi,
            "utterances": utterances,
            "provider_changes": provider_changes,
            "failed_sessions": failed_sessions,
            "anomaly_sessions": anomaly_sessions[:25],
            "flagged_sessions": flagged_sessions[:50],
            "available_pipelines": sorted(all_pipelines),
            "orchestrator_metrics": None,
        },
        "issue_inputs": issue_inputs,
    }


async def _fetch_orchestrator_overlay(days: int) -> dict | None:
    """Best-effort overlay fetch.  Returns the parsed overlay dict on success,
    None on any failure / timeout.  Caller merges into kpi."""
    try:
        cleared = rollup.get_cleared_state()
        global_cleared_at = float(cleared.get("global_cleared_at") or 0.0)
        window_s = days * 86400
        if global_cleared_at > 0:
            since_clear = max(1, int(datetime.now(UTC).timestamp() - global_cleared_at))
            if since_clear < window_s:
                window_param = f"{since_clear}s"
            else:
                window_param = f"{days * 24}h"
        else:
            window_param = f"{days * 24}h"
        async with orch_client() as client:
            resp = await client.get(
                f"{ORCHESTRATOR_URL}/api/v1/metrics",
                params={"window": window_param},
                timeout=_ANALYTICS_OVERLAY_TIMEOUT_S,
            )
            if resp.status_code != 200:
                return None
            orch_data = (resp.json() or {}).get("data") or {}
            return {
                "pipeline": orch_data.get("pipeline") or {},
                "workers_active": int(orch_data.get("workers_active") or 0),
                "window": orch_data.get("window"),
                "timestamp": orch_data.get("timestamp"),
            }
    except (TimeoutError, httpx.HTTPError, ValueError, TypeError):
        return None


def clear_analytics_data(
    *,
    pipeline: str | None = None,
    session_id: str | None = None,
    delete_captures: bool = False,
) -> dict:
    """Clear analytics state for all pipelines, a single pipeline, or one session.

    Always records the clear watermark in ``analytics_cleared.json`` and purges
    matching rows from ``rollups.db``, ``issues.db``, and the in-memory response
    cache. When ``delete_captures`` is True, also removes matching session
    directories from ``config.CAPTURES_DIR``.
    """
    import shutil

    from ..issues import clear_issues

    if config.CAPTURES_DIR == _CAPTURES_DIR_FALLBACK:
        _resolve_captures_dir()

    state = rollup.record_cleared(pipeline=pipeline, session_id=session_id)
    rollups_removed = rollup.clear_rollups(pipeline=pipeline, session_id=session_id)
    issues_removed = clear_issues(pipeline=pipeline, session_id=session_id)

    deleted_captures = 0
    if delete_captures and config.CAPTURES_DIR.exists():
        try:
            base_resolved = config.CAPTURES_DIR.resolve()
            for entry in list(config.CAPTURES_DIR.iterdir()):
                if not entry.is_dir():
                    continue
                try:
                    if entry.resolve().parent != base_resolved:
                        continue
                except OSError:
                    continue
                meta_path = entry / "metadata.json"
                sid = entry.name
                pname = ""
                if meta_path.exists():
                    try:
                        meta = json.loads(meta_path.read_text(encoding="utf-8"))
                        sid = str(meta.get("id") or entry.name)
                        pname = str(meta.get("pipeline_name") or "")
                    except (OSError, ValueError, TypeError):
                        pass
                elif session_id is None and pipeline is not None:
                    continue

                match = False
                if session_id is not None:
                    match = sid == session_id or entry.name == session_id
                elif pipeline is not None:
                    match = pname == pipeline
                else:
                    # Global clear: only remove directories that look like session captures
                    match = meta_path.exists() or (entry / "events.jsonl").exists()

                if match:
                    shutil.rmtree(entry, ignore_errors=True)
                    deleted_captures += 1
        except OSError:
            _log.warning("failed while deleting capture directories", exc_info=True)

    _analytics_cache.clear()
    return {
        "cleared": state,
        "rollups_removed": rollups_removed,
        "issues_removed": issues_removed,
        "deleted_captures": deleted_captures,
    }


"""Anomaly detection + magnitude grading.

`_detect_anomalies` produces the candidate `anomalies[]` the LLM is asked to
confirm/explain; `_anomaly_severity` grades each by magnitude so the graded
(v2) verdict can ignore near-target noise generically.
"""

from ..captures.reader import _num
from .turns import (
    _estimated_llm_wait_ms,
    _llm_has_work,
    _stt_critical_ms,
    _usable_ttft_ms,
    tool_ack_ms,
    tool_roundtrip_ms,
)

# Default multiplicative severity bands (× target). Mirrors the Rust
# `SeverityBands::default()` so the two ends agree. `ratio < warn` → negligible
# (the generic replacement for every ad-hoc "marginal/near-target" demotion).
_SEVERITY_BANDS = (1.15, 1.5, 2.5)  # (warn, moderate, severe)

# Anomaly kinds where the metric is higher-is-better (a *low* observed is the
# defect), so the overage ratio is target/observed rather than observed/target.
_HIGHER_IS_BETTER_KINDS = {"llm.throughput_low"}

# Kinds graded on the *absolute* observed value rather than × target: realtime
# factors have a hard physical meaning (1.0 = exactly real time), so the bands
# are rtf values — minor from 1.0, moderate from 1.2, severe from 1.5.
# Anything below the first band is
# negligible (never happens in practice: the rules only fire above 1.0).
_ABSOLUTE_SEVERITY_BANDS: dict[str, tuple[float, float, float]] = {
    "output.slower_than_realtime": (1.0, 1.2, 1.5),
    "client.starvation": (1.0, 1.2, 1.5),
}

# `emit_rtf − source_rtf` above which the post-audio chain — not the
# provider — is the cause of a slow emit:
# the chain then owns `output.slower_than_realtime`, and any
# `audio_post.over_budget` on the turn is graded severe.
_CHAIN_BLAME_RTF_GAP = 0.3

# `emit_rtf` at which `output.slower_than_realtime` is an `error` rather than a
# `warn`: 20 % behind real time starves a 200 ms prebuffer within a second.
_EMIT_RTF_ERROR = 1.2

# `client.jitter` severity from the silence fraction (total_silence ÷ audio):
# one glitch in a long reply is negligible; a quarter of the turn silent is not.
_JITTER_SILENCE_BANDS = (0.02, 0.10, 0.25)  # (minor, moderate, severe)

# Realtime `ttfb_anchor_source` values on which `tts.ttfb_ms` is an honest
# "end of speech → first AUDIO" measurement and may be graded.
# Excluded on purpose: `turn_start` (no
# anchor, ttfb ≈ 0) and the reader's legacy `provider` label (anchored, but
# on those captures ttfb could be stamped on the first *text* delta — a
# number known to be wrong is not graded).
_HONEST_TTFB_ANCHORS = frozenset({
    "provider_speech_stopped", "input_transcript_proxy", "vp_vad",
    "client_speech_end", "text_submit",
})

# The anchor source that is a late proxy for the end of speech (Gemini today:
# the last input-transcript delta, 0.45–1.0 s after the user stopped). When
# neither vp nor the client stamped the real end, TTFB/E2E under-report.
_PROXY_TTFB_ANCHOR = "input_transcript_proxy"


# Owner of an endpointing breach that the configured silence wait explains.
_TURN_DETECTION = "turn_detection"


def _endpointing_attribution(
    observed: float, target: float, silence_timeout_ms
) -> tuple[str | None, str]:
    """Who owns an `endpointing_ms` breach, and the sentence that says why.

    `endpointing_ms` is the wait between the speaker stopping and the turn
    committing. It has a floor nobody can beat: the pipeline waits
    `silence_timeout_ms` of silence before it will call a turn finished, so a
    700 ms timeout means at least 700 ms here however fast the recogniser is.
    The rest -- observed minus that floor -- is what the recogniser added by
    delivering its final transcript after the silence had already elapsed.

    Every breach used to be filed under the recogniser. On a session with a
    700 ms timeout and a 500 ms target that named Parakeet "the primary
    bottleneck" for turns where it had added 16 to 75 ms; the other 700 were a
    line in a config file. Blame follows the larger share of the overage:

      * the setting's share is how far the floor alone is past target;
      * the recogniser's share is everything above the floor.

    Returns `(None, ...)` for the default owner (the recogniser) and the
    turn-detection owner otherwise. With no timeout on record -- captures
    written before the engine recorded it -- nothing is known about the
    floor, and the old attribution stands rather than a guessed one.
    """
    default = "VAD endpointing took longer than target (slow turn-taking)"
    if (
        not isinstance(silence_timeout_ms, (int, float))
        or isinstance(silence_timeout_ms, bool)
        or silence_timeout_ms <= 0
    ):
        return None, default

    floor = float(silence_timeout_ms)
    added = max(0.0, float(observed) - floor)
    config_share = max(0.0, floor - float(target))
    if config_share >= added and config_share > 0:
        return _TURN_DETECTION, (
            f"turns wait {floor:.0f} ms of silence before committing "
            f"(`silence_timeout_ms`), which alone is past the {target:.0f} ms target; "
            f"the recogniser added {added:.0f} ms on top. This is configuration, not "
            f"recogniser speed -- shortening it trades against cutting speakers off "
            f"mid-thought"
        )
    return None, (
        f"the turn committed {added:.0f} ms after the configured {floor:.0f} ms "
        f"silence wait had elapsed -- the recogniser's final transcript arrived late"
    )


def _anomaly_severity(a: dict) -> str:
    """Grade an anomaly by magnitude: negligible | minor | moderate | severe.

    Additive to the existing `level` (warn/info) — the graded (v2) verdict uses
    this to ignore near-target noise generically, while v1 output is unchanged.
    `info`-level rows (already intentionally demoted) are negligible.
    """
    if a.get("level") == "info":
        return "negligible"
    tgt = a.get("target")
    # Grade on the *user-facing* magnitude. For a metric whose raw value overlaps
    # the user's speech (STT `processing_ms`), `on_path_observed` carries the
    # overlap-adjusted critical-path contribution, so severity stays consistent
    # with `on_critical_path` (both derived from the same value) instead of
    # over-grading off the raw metric.
    obs = a.get("on_path_observed")
    if obs is None:
        obs = a.get("observed")
    abs_bands = _ABSOLUTE_SEVERITY_BANDS.get(a.get("kind") or "")
    if abs_bands is not None:
        if not isinstance(obs, (int, float)):
            return "moderate"
        lo, mid, hi = abs_bands
        if obs < lo:
            return "negligible"
        if obs < mid:
            return "minor"
        if obs < hi:
            return "moderate"
        return "severe"
    if not (isinstance(obs, (int, float)) and isinstance(tgt, (int, float)) and obs >= 0 and tgt > 0):
        return "moderate"  # unknown magnitude → treat as a real, mid signal
    if obs == 0:
        # A zero on-path contribution is the best possible value for a
        # lower-is-better metric (fully overlapped / off the path) — and the
        # worst for higher-is-better. Handle explicitly: the ratio math below
        # would div-by-zero, and the old `obs > 0` guard mis-graded a zero
        # contribution as "moderate".
        return "severe" if a.get("kind") in _HIGHER_IS_BETTER_KINDS else "negligible"
    ratio = (tgt / obs) if a.get("kind") in _HIGHER_IS_BETTER_KINDS else (obs / tgt)
    warn, moderate, severe = _SEVERITY_BANDS
    if ratio < warn:
        return "negligible"
    if ratio < moderate:
        return "minor"
    if ratio < severe:
        return "moderate"
    return "severe"


def _tool_call_anomalies(
    client_tool_calls: list[dict], targets: dict[str, float]
) -> list[dict]:
    """Anomalies for client-side tool calls.

    The round-trip (dispatch → terminal result) is APPLICATION-domain time —
    media startup, UI navigation, in the playground literally a human
    resolving the command card. A long round-trip is therefore surfaced as an
    `info` diagnostic (with ack context when the client sent
    `started`/`progress`) so the report can narrate it, but it NEVER
    escalates the verdict — a tool that legitimately takes 10s must not
    grade the session degraded. The pipeline's own share of a tool exchange
    is judged by the re-anchored followup TTFB instead
    (turns._apply_tool_context).

    The one genuine `warn` here is a *gated* call that never resolves: the
    user audibly waited for a spoken confirmation that never came.
    """
    out: list[dict] = []
    target = targets.get("tool_roundtrip_ms")
    for c in client_tool_calls:
        utt = c.get("after_utterance_id")
        name = c.get("name") or "?"
        gated = bool(c.get("speak_on_result") or c.get("spoken"))
        status = c.get("status")
        ack = tool_ack_ms(c)
        ack_clause = (
            f"; client acked after {ack:.0f}ms" if ack is not None
            else "; no started/progress ack from the client"
        )
        if status == "pending":
            out.append({
                "utt": utt, "kind": "tool.unresolved",
                "level": "warn" if gated else "info",
                "observed": None, "target": None,
                "detail": (
                    f"client tool `{name}` never resolved — no result event "
                    "before session end"
                    + (
                        "; a spoken confirmation was gated on it and never played"
                        if gated else ""
                    )
                    + ack_clause
                ),
                "on_critical_path": gated,
            })
            continue
        if status == "failed":
            out.append({
                "utt": utt, "kind": "tool.failed", "level": "info",
                "observed": None, "target": None,
                "detail": f"client tool `{name}` resolved as failed",
            })
        rt = tool_roundtrip_ms(c)
        if rt is not None and isinstance(target, (int, float)) and rt > target:
            out.append({
                "utt": utt, "kind": "tool.roundtrip_high",
                "level": "info",
                "observed": round(rt, 1), "target": target,
                "detail": (
                    f"client tool `{name}` round-trip above target — app-side "
                    "execution time, not pipeline latency"
                    + (
                        "; a spoken confirmation was gated on it, so the user-felt "
                        "wait shows in the followup turn's e2e_perceived_ms"
                        if gated
                        else "; fire-and-forget (no speech gated on it)"
                    )
                    + ack_clause
                ),
                "on_critical_path": gated,
            })
    return out


def _interrupted_by_the_caller(turn: dict | None) -> bool:
    """Whether the server VAD cut this turn's reply off for the caller's speech."""
    if not turn or not turn.get("interrupted"):
        return False
    return (turn.get("bi") or {}).get("trigger") == "server_vad"


def _detect_anomalies(
    turns: list[dict],
    targets: dict[str, float],
    client_tool_calls: list[dict] | None = None,
) -> list[dict]:
    """Compute the anomaly list the LLM is asked to confirm/explain."""
    anomalies: list[dict] = []

    # Gather totals for outlier check (skip terminated and agent-initiated
    # turns — agent turns are anchored at turn-open, not end-of-user-speech,
    # so mixing them in distorts the outlier baseline).
    totals = sorted(
        t["total_ms"] for t in turns
        if isinstance(t.get("total_ms"), (int, float))
        and not t.get("terminated")
        and t.get("initiator") != "agent"
    )
    median_total = totals[len(totals) // 2] if totals else None
    outlier_factor = float(targets.get("outlier_total_ms_factor", 2.0))
    by_utt = {t.get("utt"): t for t in turns}

    for t in turns:
        utt = t.get("utt")
        # Skip cold-start turn for latency thresholds (informational).
        #
        # Each stage carries its own `cold_start`, and they warm up
        # independently: a session can reach a warm recogniser through a cold
        # model or the reverse. Reading only the model's flag silenced every
        # other stage's latency rule whenever the model happened to be cold --
        # a 10.4 s recogniser stall went unreported because the model beside
        # it was on its first turn. Cold is a property of the stage that was
        # cold, so ask each one.
        is_cold = bool((t.get("llm") or {}).get("cold_start"))
        stt_is_cold = bool((t.get("stt") or {}).get("cold_start"))

        def add(kind: str, observed, target, detail: str, level: str = "warn",
                on_critical_path: bool | None = None, on_path_observed=None,
                component: str | None = None, severity: str | None = None,
                anchor_source: str | None = None):
            a = {
                "utt": utt, "kind": kind, "level": level,  # noqa: B023 - `add` is used only within this iteration
                "observed": observed, "target": target, "detail": detail,
            }
            # Provenance of the anchored metric behind this row (S2S TTFB /
            # endpointing): lets the verdict headline say what the number
            # was measured from.
            if anchor_source is not None:
                a["anchor_source"] = anchor_source
            # Additive: whether this breach is on the user-facing critical
            # path. `None` = always/unconditionally (the default). Only the
            # graded (v2) verdict reads it; `level` is untouched so v1 output and
            # the golden replay are unchanged.
            if on_critical_path is not None:
                a["on_critical_path"] = on_critical_path
            # The overlap-adjusted, user-facing magnitude (e.g. STT critical-path
            # contribution vs raw processing_ms). Severity is graded on this so it
            # stays consistent with `on_critical_path`.
            if on_path_observed is not None:
                a["on_path_observed"] = round(float(on_path_observed), 1)
            # Per-anomaly blame override for kinds whose owner is decided by
            # the data (a slow emit is the chain's or the provider's; a
            # post-processor / annotator is named `audio_post:<name>` /
            # `annotator:<name>`). The verdict prefers this over its static
            # kind → component table.
            if component is not None:
                a["component"] = component
            # Pre-graded severity for rules whose magnitude isn't a plain
            # observed ÷ target ratio; the generic grader below fills the rest.
            if severity is not None:
                a["severity"] = severity
            anomalies.append(a)

        stt = t.get("stt") or {}
        llm = t.get("llm") or {}
        tts = t.get("tts") or {}
        anchor = t.get("anchor") or {}
        ttfb_anchor_src = anchor.get("ttfb_anchor_source")

        # Was the caller heard and then ignored?
        #
        # Speech that reached the recogniser and produced no turn is the
        # worst outcome a voice agent has short of crashing: the caller says
        # something, nothing happens, and they say it again. Nothing else in
        # the report shows it -- there is no turn to be slow -- so the wait
        # appears only as a long `e2e_perceived_ms` on the turn that finally
        # answered. Which of the two rules fires is decided by whether the
        # recogniser had words and lost them (its fault) or never had any (a
        # sound the VAD let through, which is the system declining to answer
        # noise, and only worth a note).
        for i, d in enumerate(t.get("dropped_before") or []):
            text = (d.get("text") or "").strip()
            audio_ms = d.get("audio_ms")
            if d.get("had_interim_text"):
                add("stt.utterance_dropped", audio_ms, 0,
                    "the recogniser produced words for this speech and then returned "
                    "nothing for it, so no turn was made and the caller was not answered",
                    level="error", severity="severe", on_critical_path=True, component="stt")
            elif i == 0 and _interrupted_by_the_caller(by_utt.get(utt - 1) if isinstance(utt, int) else None):
                # The speech that cut the previous reply off is the first
                # thing after it, so the first drop here is that speech. The
                # VAD confirmed it as the caller talking over the agent, so
                # a sound with no words in it is the recogniser's miss, not
                # noise: the caller cut the agent off and was not answered.
                add("stt.interruption_unheard", audio_ms, 0,
                    "the speech that interrupted the previous reply was heard by the VAD "
                    "but the recogniser found no words in it, so the reply was cut off "
                    "and nothing answered; the caller saw the agent fall silent",
                    level="warn", severity="moderate", on_critical_path=True, component="stt")
            elif d.get("reason") == "too_short" and text:
                add("stt.utterance_dropped", audio_ms, 0,
                    f"speech was transcribed as {text!r}, under the minimum length, and discarded",
                    level="info", severity="negligible", on_critical_path=False, component="stt")
            else:
                add("vad.non_speech_forwarded", audio_ms, 0,
                    "the VAD forwarded a sound the recogniser found no words in; "
                    "discarded, which is correct, but it cost a recogniser pass",
                    level="info", severity="negligible", on_critical_path=False, component="vad")

        # Did the turn carry any words at all?
        #
        # Every other rule here grades how fast a stage was. None of them ask
        # whether what came out meant anything, so a turn that committed on a
        # transcript of ".." graded green on each stage and the session was
        # reported healthy while the conversation was plainly broken. The
        # recogniser returning punctuation for speech is a failure whatever
        # the timings say.
        #
        # Deliberately outside the cold-start skip below: warm-up explains a
        # slow turn, never an empty one. Agent-initiated turns have no user
        # speech by definition and are exempt, as are terminated turns, where
        # a missing transcript is the symptom of the termination and is
        # already reported.
        if t.get("initiator") != "agent" and not t.get("terminated"):
            transcript = t.get("user_transcript")
            if isinstance(transcript, str):
                spoken = "".join(c for c in transcript if c.isalnum())
                if not spoken:
                    shown = transcript.strip() or "(empty)"
                    add(
                        "stt.transcript_empty",
                        len(spoken), 1,
                        f"the turn committed on a transcript with no words: {shown!r} — "
                        f"speech reached the recogniser and came back as nothing",
                        level="error", severity="severe",
                        on_critical_path=True, component="stt",
                    )

        raw_ttft = _num(llm.get("ttft_ms"))
        usable_ttft = _usable_ttft_ms(llm)
        est_llm_wait = _estimated_llm_wait_ms(llm, tts)
        eff_ttft = usable_ttft if usable_ttft is not None else est_llm_wait
        raw_ttfb = _num(tts.get("ttfb_ms"))
        internal_ttfb = _num(tts.get("internal_ttfb_ms"))
        audio_dur = _num(tts.get("audio_duration_ms"))

        # Plausibility / measurement integrity rules (§4.4).
        if raw_ttft is not None and raw_ttft <= 0 and _llm_has_work(llm):
            add(
                "measurement.zero_with_work",
                raw_ttft,
                None,
                "LLM reported ttft_ms=0 while reporting non-zero work (tokens or prompt ingest/generation time); "
                "value treated as missing rather than attributing the wait to downstream TTS",
                level="warn",
                severity="negligible",
                on_critical_path=False,
                component="measurement",
            )
        if raw_ttfb is not None and raw_ttfb <= 0 and audio_dur is not None and audio_dur > 0:
            add(
                "measurement.zero_with_work",
                raw_ttfb,
                None,
                "TTS reported ttfb_ms=0 while producing non-zero audio; value treated as missing",
                level="warn",
                severity="negligible",
                on_critical_path=False,
                component="measurement",
            )
        if raw_ttfb is not None and internal_ttfb is not None and internal_ttfb > raw_ttfb:
            add(
                "measurement.breakdown_exceeds_stage",
                internal_ttfb,
                raw_ttfb,
                f"TTS internal_ttfb_ms ({internal_ttfb:.1f}ms) exceeds outer ttfb_ms ({raw_ttfb:.1f}ms)",
                level="warn",
                severity="negligible",
                on_critical_path=False,
                component="measurement",
            )
        if raw_ttfb is not None and internal_ttfb is not None and usable_ttft is not None and usable_ttft > raw_ttfb:
            add(
                "measurement.breakdown_exceeds_stage",
                usable_ttft,
                raw_ttfb,
                f"LLM ttft_ms ({usable_ttft:.1f}ms) exceeds commit-anchored tts.ttfb_ms ({raw_ttfb:.1f}ms)",
                level="warn",
                severity="negligible",
                on_critical_path=False,
                component="measurement",
            )
        llm_wait = _num(llm.get("llm_wait_ms"))
        tts_wait = _num(tts.get("tts_wait_ms"))
        output_wait = _num(tts.get("output_wait_ms"))
        commit_fa = _num(tts.get("commit_to_first_audio_ms"))
        if (
            commit_fa is not None
            and commit_fa > 0
            and any(v is not None for v in (llm_wait, tts_wait, output_wait))
        ):
            stage_sum = (llm_wait or 0.0) + (tts_wait or 0.0) + (output_wait or 0.0)
            diff = abs(stage_sum - commit_fa)
            if diff > 50.0 and (diff / commit_fa) > 0.05:
                add(
                    "measurement.stages_disagree",
                    round(stage_sum, 1),
                    round(commit_fa, 1),
                    f"Telescoping stage sum ({stage_sum:.1f}ms) disagrees with "
                    f"commit_to_first_audio_ms ({commit_fa:.1f}ms) by {diff:.1f}ms",
                    level="warn",
                    severity="negligible",
                    on_critical_path=False,
                    component="measurement",
                )

        # Latency targets (skip cold-start turn).
        if not is_cold:
            # If TTFT was estimated from llm_detail.first_token_ms (cold-turn
            # fallback) we still surface it as informational rather than warn.
            # When `ttft_ms` was a 0-with-work measurement fault, fall back to
            # `_estimated_llm_wait_ms` (from `perceived_ttft_ms` / `prompt_ingest_ms`)
            # so a multi-second LLM prefill stall is still flagged on `llm`.
            if usable_ttft is not None:
                ttft_ms: float | None = usable_ttft
                ttft_estimated = bool(llm.get("ttft_estimated"))
            elif raw_ttft is not None and raw_ttft <= 0 and est_llm_wait is not None:
                ttft_ms = est_llm_wait
                ttft_estimated = False
            else:
                ttft_ms = None
                ttft_estimated = False
            if isinstance(ttft_ms, (int, float)) and ttft_ms > targets["ttft_ms"]:
                # Provider-network demotion: when the TLS/TCP connect
                # dominated TTFT (cloud cold socket on a fresh turn),
                # the model itself wasn't slow.  Surface as info and
                # name the real cause so the verdict reducer doesn't
                # blame the LLM for a transport-side hiccup.
                connect_ms = llm.get("connect_ms")
                first_token_ms = llm.get("first_token_ms") or ttft_ms
                connect_dominant = (
                    isinstance(connect_ms, (int, float))
                    and isinstance(first_token_ms, (int, float))
                    and first_token_ms > 0
                    and connect_ms / first_token_ms > 0.9
                )
                # Marginal overage (<=5% above target) is within normal
                # cloud LLM jitter; surface as info, not warn.
                marginal = (ttft_ms / targets["ttft_ms"]) <= 1.05
                if connect_dominant:
                    detail = (
                        "LLM time-to-first-token above target — but "
                        f"connect_ms ({connect_ms:.0f}ms) dominated "
                        f"first_token_ms ({first_token_ms:.0f}ms); "
                        "time-to-first-response (server prefill / routing), "
                        "not the model's incremental decode"
                    )
                    level = "info"
                elif ttft_estimated:
                    detail = "LLM time-to-first-token above target (estimated)"
                    level = "info"
                elif marginal:
                    detail = (
                        "LLM time-to-first-token marginally above target "
                        f"({ttft_ms:.0f}ms vs {targets['ttft_ms']}ms target, "
                        "<=5% over)"
                    )
                    level = "info"
                else:
                    detail = "LLM time-to-first-token above target"
                    level = "warn"
                add("llm.ttft_high", ttft_ms, targets["ttft_ms"], detail, level=level)
            # Skip TTS ttfb on interrupted turns: synthesis was cut short
            # (`synthesis_ms` is null) so `ttfb_ms` may include
            # interrupt-handling stalls rather than real TTS slowness.
            tts_synthesis_complete = isinstance(tts.get("synthesis_ms"), (int, float))
            # Realtime S2S has no `synthesis_ms` at all: the turn is
            # synthesis-complete when it was
            # not interrupted and produced audio — provided its `ttfb_ms` is
            # honestly anchored (end of speech → first AUDIO delta), which
            # only the wire `ttfb_anchor_source` guarantees.
            audio_ms = tts.get("audio_duration_ms")
            s2s_synthesis_complete = (
                ttfb_anchor_src in _HONEST_TTFB_ANCHORS
                and not t.get("interrupted")
                and isinstance(audio_ms, (int, float)) and audio_ms > 0
            )
            if (
                (tts_synthesis_complete or s2s_synthesis_complete)
                and isinstance(tts.get("ttfb_ms"), (int, float))
                and tts["ttfb_ms"] > targets["ttfb_ms"]
            ):
                # The canonical `ttfb_ms` is runner-observed: utterance-commit →
                # first audio chunk to the client, so it *contains* LLM TTFT
                # (marker: `internal_ttfb_ms` present — see critical_path_latency).
                # Judge TTS on its post-first-token share, or a slow LLM turn
                # echoes into a phantom "severe" TTS breach (same stall counted
                # twice in the verdict). Never subtract a 0-with-work `ttft_ms`
                # and charge the whole wait to TTS (P1.1/P1.2).
                ttfb_ms = tts["ttfb_ms"]
                if tts.get("internal_ttfb_ms") is not None:
                    if isinstance(eff_ttft, (int, float)) and eff_ttft <= float(ttfb_ms):
                        share = max(float(ttfb_ms) - float(eff_ttft), 0.0)
                        add("tts.ttfb_high", ttfb_ms, targets["ttfb_ms"],
                            "TTS time-to-first-byte above target (commit-anchored: "
                            f"includes LLM TTFT {eff_ttft:.0f}ms; "
                            f"TTS-side share ~{share:.0f}ms)",
                            on_critical_path=(share > targets["ttfb_ms"]),
                            on_path_observed=share)
                    elif eff_ttft is None and _llm_has_work(llm):
                        if internal_ttfb is not None and 0 < internal_ttfb < float(ttfb_ms):
                            share = float(internal_ttfb)
                            add("tts.ttfb_high", ttfb_ms, targets["ttfb_ms"],
                                "TTS time-to-first-byte above target (commit-anchored with "
                                f"unusable LLM TTFT; TTS internal share ~{share:.0f}ms)",
                                on_critical_path=(share > targets["ttfb_ms"]),
                                on_path_observed=share)
                    elif s2s_synthesis_complete and not tts_synthesis_complete:
                        severe_from = _SEVERITY_BANDS[2] * float(targets["ttfb_ms"])
                        add("tts.ttfb_high", ttfb_ms, targets["ttfb_ms"],
                            f"speech-to-speech model: end of speech ({ttfb_anchor_src}) → first audio",
                            level="error" if float(ttfb_ms) >= severe_from else "warn",
                            on_critical_path=True, anchor_source=ttfb_anchor_src)
                    else:
                        add("tts.ttfb_high", ttfb_ms, targets["ttfb_ms"],
                            "TTS time-to-first-byte above target")
                elif s2s_synthesis_complete and not tts_synthesis_complete:
                    # One model IS the whole stack: this is the model's entire
                    # response latency from the end of speech (the verdict
                    # collapses the `tts` owner to `s2s`). `error` from the
                    # severe band up (a 1.6 s wait is not a "warn"); the
                    # cascade rule keeps its plain `warn`.
                    severe_from = _SEVERITY_BANDS[2] * float(targets["ttfb_ms"])
                    add("tts.ttfb_high", ttfb_ms, targets["ttfb_ms"],
                        f"speech-to-speech model: end of speech ({ttfb_anchor_src}) → first audio",
                        level="error" if float(ttfb_ms) >= severe_from else "warn",
                        on_critical_path=True, anchor_source=ttfb_anchor_src)
                else:
                    add("tts.ttfb_high", ttfb_ms, targets["ttfb_ms"],
                        "TTS time-to-first-byte above target")
        # The recogniser's own rules, gated on the recogniser's own warm-up.
        # Nested under the model's `is_cold` they were unreachable whenever
        # the model was cold, which is most first turns.
        if not stt_is_cold:
            # Endpointing. S2S: `stt.endpointing_ms` IS `provider_endpointing_ms`
            # (turns._flatten_turn) — how late the provider's VAD/transcription
            # noticed the end of speech relative to vp's own VAD. User-felt
            # wait, on the critical path, same kind as the cascade rule so the
            # owner table / dashboards need no new entry (the s2s collapse
            # moves the owner from `stt` to the model).
            provider_endpt = anchor.get("provider_endpointing_ms")
            if isinstance(provider_endpt, (int, float)) and provider_endpt > targets["endpointing_ms"]:
                add("stt.endpointing_high", provider_endpt, targets["endpointing_ms"],
                    f"provider VAD/transcription noticed the end of speech "
                    f"{provider_endpt:.0f} ms late (anchor: {ttfb_anchor_src})",
                    on_critical_path=True, anchor_source=ttfb_anchor_src)
            elif isinstance(stt.get("endpointing_ms"), (int, float)) and stt["endpointing_ms"] > targets["endpointing_ms"]:
                owner, why = _endpointing_attribution(
                    stt["endpointing_ms"], targets["endpointing_ms"], stt.get("silence_timeout_ms")
                )
                add("stt.endpointing_high", stt["endpointing_ms"], targets["endpointing_ms"],
                    why, component=owner)
                if owner == _TURN_DETECTION:
                    # Carried so the headline can name the setting and its value.
                    anomalies[-1]["silence_timeout_ms"] = stt["silence_timeout_ms"]
            # Skip processing-time anomaly for streaming STT (metric n/a).
            if (
                stt.get("mode") != "streaming"
                and isinstance(stt.get("processing_ms"), (int, float))
                and stt["processing_ms"] > targets["stt_processing_ms"]
            ):
                # `processing_ms` overlaps the user's speech for incremental
                # models (RTFx > 1), so the raw value over-states the user wait.
                # The graded verdict judges the on-critical-path share instead:
                # this breach only counts when the *post-speech* STT contribution
                # (`_stt_critical_ms`) itself exceeds the target. v1 still flags
                # it as warn (unchanged); v2 uses `on_critical_path` to ignore
                # the overlap false-positive generically (no provider name).
                stt_crit = _stt_critical_ms(stt)
                add("stt.processing_high", stt["processing_ms"], targets["stt_processing_ms"],
                    "STT transcription latency above target",
                    on_critical_path=(stt_crit > targets["stt_processing_ms"]),
                    # Grade severity on the on-critical-path share, not the raw
                    # (overlapping) processing_ms — keeps severity consistent with
                    # on_critical_path so a big-but-overlapped stall isn't "severe".
                    on_path_observed=stt_crit)

        # Anchor provenance (info only, never affects the level; not subject
        # to the cold-start skip — provenance is not a latency): the turn's
        # TTFB/E2E start at a late proxy for the end of speech and nothing
        # better (vp VAD / client VAD stamp) exists to correct it — the
        # real wait is longer than every number on this row says.
        if (
            ttfb_anchor_src == _PROXY_TTFB_ANCHOR
            and anchor.get("vp_speech_end_epoch_ms") is None
            and anchor.get("client_speech_end_epoch_ms") is None
        ):
            add("s2s.anchor_proxy", tts.get("ttfb_ms"), None,
                "TTFB/E2E measured from a late proxy (provider input transcript); "
                "real wait is longer",
                level="info", component="s2s", anchor_source=ttfb_anchor_src)

        # RTF / throughput.  RTF is normally intrinsic (text length +
        # model decode speed), but the *first* synthesis pays a model
        # warm-up cost that inflates RTF on the cold-start turn — e.g.
        # rtf 1.88 cold vs 0.49 warm for the same provider in one
        # session.  Demote cold-start RTF to info so a warm-up blip does
        # not pin the verdict, matching the cold-start exemption the
        # latency metrics above already get.
        tts_cold = bool(tts.get("cold_start"))
        if isinstance(tts.get("rtf"), (int, float)) and tts["rtf"] > targets["rtf_max"]:
            if tts_cold:
                add("tts.rtf_high", tts["rtf"], targets["rtf_max"],
                    "TTS realtime-factor above target on the cold-start turn "
                    "(first-synthesis model warm-up, not steady-state "
                    "synthesis speed)",
                    level="info")
            else:
                add("tts.rtf_high", tts["rtf"], targets["rtf_max"],
                    "TTS realtime-factor above target (synthesis slower than playback)")
        # tokens/sec is misleading on cold turns (TLS handshake dominates
        # `generation_ms`) and on tiny outputs (<30 tokens) where the
        # per-request startup cost swamps the per-token decode rate. Skip
        # both — the throughput target is calibrated for steady-state
        # decode, not for short replies.
        completion_tokens = llm.get("completion_tokens")
        throughput_eligible = (
            not is_cold
            and isinstance(completion_tokens, int)
            and completion_tokens >= 30
        )
        if (
            throughput_eligible
            and isinstance(llm.get("tokens_per_sec"), (int, float))
            and isinstance(llm.get("generation_ms"), (int, float))
            and llm["generation_ms"] > 200
            and llm["tokens_per_sec"] < targets["tokens_per_sec_min"]
        ):
            add("llm.throughput_low", llm["tokens_per_sec"], targets["tokens_per_sec_min"],
                "LLM token throughput below target")

        # The wait the caller felt, against the wait the server timed.
        #
        # `critical_path.total_ms` starts when the server decided the caller
        # had stopped speaking. `e2e_perceived_ms` starts when the caller's
        # own device saw them stop. When the two differ by seconds, the
        # server's clock started late: the first attempt to speak was lost
        # and this turn answers the second. Only the client's figure knows,
        # so only it can raise this, and it is user-felt by definition.
        #
        # The client's mark can also be early rather than the server's late:
        # its detector takes a pause for the end of speech and the caller
        # goes on talking. Then the recogniser holds audio for the whole
        # stretch, and the gap between the two clocks is speech, not a wait.
        # So the gap is measured net of the audio the recogniser was given
        # beyond the client's own estimate of the utterance.
        felt = t.get("e2e_perceived_ms")
        timed = (t.get("critical_path") or {}).get("total_ms")
        heard_ms = stt.get("audio_duration_ms")
        gap = (felt - timed) if isinstance(felt, (int, float)) and isinstance(timed, (int, float)) else None
        if gap is not None and isinstance(heard_ms, (int, float)):
            # Audio the server heard past the client's end mark cannot have
            # been a wait. `heard_ms` is the whole utterance, so this is a
            # generous discount; a genuine lost turn is seconds beyond it.
            gap -= max(0.0, float(heard_ms))
        # A spoken tool follow-up was gated on the client's own tool
        # round-trip (media starting, a screen loading). The caller did wait
        # for it, but it is application time, already excluded from this
        # turn's latency, not a lost first attempt to speak.
        excluded = (t.get("tool_context") or {}).get("excluded_wait_ms")
        if gap is not None and isinstance(excluded, (int, float)):
            gap -= max(0.0, float(excluded))
        if (
            not t.get("e2e_estimated")
            and t.get("initiator") != "agent"
            and gap is not None
            and gap > targets["unanswered_wait_ms"]
        ):
            waited = (f"the caller waited {felt / 1000:.1f} s for this reply while the server "
                      f"timed {timed / 1000:.1f} s")
            dropped = t.get("dropped_before") or []
            if not dropped:
                detail = (f"{waited}: the server's clock started late, which is what a lost "
                          "first attempt to speak looks like")
                severity = "severe"
            elif any(d.get("had_interim_text") for d in dropped):
                detail = (f"{waited}: the recogniser produced words for an earlier attempt and "
                          "then returned nothing for it, so the attempt was lost")
                severity = "severe"
            else:
                # The wait is accounted for: sounds the VAD forwarded that
                # came back without words. Whether they were noise the caller
                # made or speech the recogniser missed, only the dropped
                # recordings in the capture can tell, so this is graded as a
                # wait the caller felt, not as a lost attempt proven.
                lengths = ", ".join(
                    f"{(d.get('audio_ms') or 0) / 1000:.1f} s" for d in dropped
                )
                detail = (f"{waited}: the wait spans {len(dropped)} sound(s) the recogniser "
                          f"found no words in ({lengths}), either noise the caller made or "
                          "speech it missed; the dropped recordings in the capture tell which")
                severity = "moderate"
            add("turn.unanswered_wait", round(felt), round(timed + targets["unanswered_wait_ms"]),
                detail, level="error" if severity == "severe" else "warn", severity=severity,
                on_critical_path=True,
                # The stage that turns speech into a turn is the recogniser;
                # a first attempt that never became one is charged to it. A
                # capture from before dropped utterances were recorded shows
                # the same shape, and the same charge is the likeliest.
                component="stt")

        # A tool-invocation turn has no spoken reply and so no
        # `e2e_perceived_ms`; its wait is speech end to the tool call's
        # dispatch. A model that sat on a clean request for twenty seconds
        # before calling the tool left the caller in exactly the silence the
        # rule above exists for, and without this it blamed the client tool
        # that was still waiting to be resolved. The turn's `e2e_estimated`
        # says nothing here (there is no e2e to estimate); the speech-end
        # stamps below are wall-clock marks the client or the worker made.
        if t.get("kind") == "tool_invocation":
            speech_end = next(
                (
                    anchor.get(k)
                    for k in ("client_speech_end_epoch_ms", "vp_speech_end_epoch_ms")
                    if isinstance(anchor.get(k), (int, float))
                ),
                None,
            )
            own = [
                c for c in (client_tool_calls or [])
                if c.get("after_utterance_id") == utt
                and isinstance(c.get("called_at_ms"), (int, float))
            ]
            if speech_end is not None and own:
                first = min(own, key=lambda c: float(c["called_at_ms"]))
                wait = float(first["called_at_ms"]) - float(speech_end)
                if wait > targets["unanswered_wait_ms"]:
                    add("turn.unanswered_wait", round(wait), targets["unanswered_wait_ms"],
                        f"the caller waited {wait / 1000:.1f} s after speech ended before the "
                        f"model dispatched the tool call `{first.get('name') or '?'}`; nothing "
                        "was spoken in between, so the wait is the model's, not the tool's",
                        level="error", severity="severe", on_critical_path=True,
                        # The stage that decides to act on the request; for a
                        # speech-to-speech model the verdict collapses it to
                        # the model.
                        component="llm")

        # Total-time outlier. Agent-initiated turns are excluded: their
        # total is turn-open-anchored, not comparable to user turns.
        if (
            median_total
            and isinstance(t.get("total_ms"), (int, float))
            and t["total_ms"] > outlier_factor * median_total
            and not t.get("terminated")
            and t.get("initiator") != "agent"
        ):
            add("turn.total_outlier", t["total_ms"], round(outlier_factor * median_total),
                f"Turn total_ms is >{outlier_factor}x session median",
                level="info")

        # Suspicious barge-in: very small played_ms suggests echo/self-barge.
        bi = t.get("bi") or {}
        if (
            t.get("interrupted")
            and isinstance(bi.get("played_ms"), (int, float))
            and bi["played_ms"] < targets["self_barge_played_ms_max"]
            and bi.get("trigger") == "server_vad"
        ):
            add("bi.likely_self_barge", bi["played_ms"], targets["self_barge_played_ms_max"],
                "server_vad barge-in fired with very low played_ms — likely echo/self-barge",
                level="info")

        # Truncated synthesis: chars present but audio missing.
        if (
            isinstance(tts.get("chars"), int) and tts["chars"] > 5
            and isinstance(tts.get("audio_duration_ms"), int) and tts["audio_duration_ms"] < 100
            and not t.get("interrupted")
        ):
            add("tts.audio_truncated", tts["audio_duration_ms"], 100,
                "TTS produced text but almost no audio — possible synthesis failure")

        # Barge-in RTT.
        rtt_ms = bi.get("barge_in_rtt_ms")
        if isinstance(rtt_ms, (int, float)) and rtt_ms > targets["barge_in_rtt_ms"]:
            # A real user barge-in (played_ms well above the self-barge
            # threshold) with only a small overage (<=1.5x target) is
            # within normal network jitter — the user already heard a
            # meaningful chunk of the bot's reply, and ~100-200ms of
            # extra tail isn't perceptually distinct.  Surface as info
            # so a single such event doesn't pin the verdict to
            # "degraded / transport bottleneck".  Sub-second overages
            # on short-played turns remain warn.
            played_ms = bi.get("played_ms")
            real_user_barge = (
                isinstance(played_ms, (int, float)) and played_ms >= 1000.0
            )
            small_overage = (rtt_ms / targets["barge_in_rtt_ms"]) <= 1.5
            if real_user_barge and small_overage:
                add("bi.rtt_high", rtt_ms, targets["barge_in_rtt_ms"],
                    f"Barge-in round-trip {rtt_ms:.0f}ms vs "
                    f"{targets['barge_in_rtt_ms']}ms target — user heard "
                    f"{int(played_ms)}ms of audio first (real user barge-in, "
                    "<=1.5x overage within network jitter)",
                    level="info")
            else:
                add("bi.rtt_high", rtt_ms, targets["barge_in_rtt_ms"],
                    "Barge-in round-trip above target (user hears stale TTS)")

        _throughput_anomalies(t, targets, add)

    # Client-tool round-trips are per-call, not per-turn (a turn can dispatch
    # several); they attach to the dispatching turn via `after_utterance_id`.
    if client_tool_calls:
        anomalies.extend(_tool_call_anomalies(client_tool_calls, targets))

    # Grade every anomaly by magnitude (additive; does not change `level`).
    # Rules that pre-graded themselves (chain-caused post-processor breaches,
    # jitter by silence fraction, annotator outages) keep their grade.
    for a in anomalies:
        if "severity" not in a:
            a["severity"] = _anomaly_severity(a)
    return anomalies


def _throughput_anomalies(t: dict, targets: dict[str, float], add) -> None:
    """Throughput + playback + annotator rules for one flattened turn.

    vp measured latency and failures; these rules see a stream that starts on
    time and then trickles. All keyed off the row blocks `_flatten_turn` adds
    from the `output` / `audio_post:*` / `annotator:*` components and the
    client's playback frame — absent on old captures, so nothing fires there.

      output.slower_than_realtime  emit_rtf > 1.0 (critical path; error ≥ 1.2)
      audio_post.over_budget       any processor budget_ratio > 1.0 or timeouts > 0
      client.starvation            underruns > 0 and arrival_rtf > 1.0  → server output
      client.jitter                underruns > 0 and arrival_rtf ≤ 1.0  → network/prebuffer
      annotator.lagging            lag_max_ms > budget or client annotation_late_ms_max > 0
      annotator.degraded           timeouts > 0 or dropped_chunks > 0
      annotator.unavailable        breaker_open

    Blame for a slow emit follows `emit_rtf − source_rtf`: above
    `_CHAIN_BLAME_RTF_GAP`, on a turn with post-processors, the post-audio
    chain is the cause (component `audio_post:<worst processor>`), otherwise
    the provider (`tts`, which the verdict collapses to `s2s` for
    speech-to-speech pipelines).
    """
    out = t.get("output") or {}
    posts: dict = t.get("audio_post") or {}
    playback = t.get("playback") or {}
    annotators: dict = t.get("annotators") or {}

    emit_rtf = _num(out.get("emit_rtf"))
    source_rtf = _num(out.get("source_rtf"))
    chain_ms = _num(out.get("chain_overhead_ms"))
    audio_ms = _num(out.get("audio_output_ms"))
    chain_gap = (emit_rtf - source_rtf) if (emit_rtf is not None and source_rtf is not None) else None
    # With no post-processor configured, the gap between source and emit is
    # the pacer and the transport, not a chain; blaming one would send the
    # reader after a processor that does not exist.
    chain_is_cause = bool(posts) and chain_gap is not None and chain_gap > _CHAIN_BLAME_RTF_GAP

    # The processor most likely responsible for a chain-caused slow emit: the
    # one furthest over budget, then the one timing out.
    worst_post: str | None = None
    if posts:
        worst_post = max(
            posts,
            key=lambda n: (
                _num((posts[n] or {}).get("budget_ratio")) or 0.0,
                _num((posts[n] or {}).get("timeouts")) or 0.0,
            ),
        )
    if chain_is_cause:
        output_component = f"audio_post:{worst_post}" if worst_post else "audio_post"
    else:
        output_component = "tts"

    # ── output.slower_than_realtime ──
    if emit_rtf is not None and emit_rtf > 1.0:
        parts = [f"emit_rtf {emit_rtf:.2f}"]
        if source_rtf is not None:
            parts.append(f"source_rtf {source_rtf:.2f}")
        if chain_gap is not None:
            parts.append(f"chain +{chain_gap:.2f}")
        if chain_ms is not None and posts:
            parts.append(f"chain_overhead {chain_ms:.0f}ms")
        pacer_ms = _num(out.get("pacer_wait_ms"))
        if pacer_ms:
            parts.append(f"pacer_wait {pacer_ms:.0f}ms")
        if audio_ms is not None and audio_ms > 0:
            audio_s = audio_ms / 1000.0
            head = f"server emitted {audio_s:.1f}s of audio in {emit_rtf * audio_s:.1f}s"
        else:
            head = "server emitted audio slower than real time"
        if chain_is_cause:
            cause = " — post-audio chain is the cause, not the provider"
        elif source_rtf is not None and source_rtf > 1.0:
            cause = " — provider/TTS delivered slower than real time"
        elif source_rtf is not None:
            cause = (" — the provider delivered in time and no post-processor ran; "
                     "the delay is between receiving the audio and sending it")
        else:
            cause = ""
        add("output.slower_than_realtime", round(emit_rtf, 2), targets.get("emit_rtf_max", 0.9),
            f"{head} ({', '.join(parts)}) — client will starve after the prebuffer{cause}",
            level="error" if emit_rtf >= _EMIT_RTF_ERROR else "warn",
            on_critical_path=True, component=output_component)

    # ── audio_post.over_budget ──
    post_target = targets.get("post_budget_ratio_max", 0.8)
    for name, p in posts.items():
        p = p or {}
        ratio = _num(p.get("budget_ratio"))
        timeouts = int(_num(p.get("timeouts")) or 0)
        if not ((ratio is not None and ratio > 1.0) or timeouts > 0):
            continue
        p50 = _num(p.get("latency_p50_ms"))
        if p50 is not None and ratio is not None and ratio > 0:
            body = f"p50 {p50:.0f}ms per {p50 / ratio:.0f}ms chunk (ratio {ratio:.2f})"
        elif ratio is not None:
            body = f"budget ratio {ratio:.2f}"
        else:
            body = "no latency sample"
        if chain_gap is not None:
            match = (
                f" — matches emit_rtf − source_rtf = +{chain_gap:.2f}"
                if chain_is_cause else
                f"; emit_rtf − source_rtf = +{chain_gap:.2f} (chain share not material)"
            )
        else:
            match = ""
        if chain_is_cause:
            severity = "severe"
        elif ratio is None or ratio <= 1.0:
            severity = "moderate"       # timeouts alone: real, mid-magnitude
        else:
            severity = None             # graded on ratio ÷ target
        add("audio_post.over_budget",
            round(ratio, 2) if ratio is not None else timeouts,
            post_target if ratio is not None else 0,
            f"post-processor {name}: {body}; {timeouts} timeouts{match}",
            level="error", on_critical_path=True,
            component=f"audio_post:{name}", severity=severity)

    # ── client.starvation vs client.jitter ──
    underruns = int(_num(playback.get("underruns")) or 0)
    if underruns > 0:
        arr = _num(playback.get("arrival_rtf"))
        silence = _num(playback.get("total_silence_ms"))
        pb_audio = _num(playback.get("audio_ms"))
        silence_clause = f", {silence:.0f}ms of silence" if silence else ""
        if arr is not None and arr > 1.0:
            add("client.starvation", round(arr, 2), 1.0,
                f"{underruns} underrun(s){silence_clause}: audio arrived slower than playback "
                f"(arrival_rtf {arr:.2f}) — server output starvation, not network jitter",
                level="warn", on_critical_path=True, component=output_component)
        elif arr is None and emit_rtf is not None and emit_rtf > 1.0:
            # Old client (no arrival_wall_ms): fall back to the server's own
            # emit_rtf as the discriminator.
            add("client.starvation", round(emit_rtf, 2), 1.0,
                f"{underruns} underrun(s){silence_clause} with emit_rtf {emit_rtf:.2f} "
                "(client sent no arrival_wall_ms) — server output starvation",
                level="warn", on_critical_path=True, component=output_component)
        else:
            if silence is not None and pb_audio and pb_audio > 0:
                frac = silence / pb_audio
                lo, mid, hi = _JITTER_SILENCE_BANDS
                severity = (
                    "negligible" if frac < lo else
                    "minor" if frac < mid else
                    "moderate" if frac < hi else "severe"
                )
            else:
                severity = "moderate"
            if arr is not None:
                why = (
                    f"arrival_rtf {arr:.2f} <= 1.0 — audio arrived on time on average; "
                    "bursty delivery vs. the client prebuffer (network / prebuffer), "
                    "not server throughput"
                )
                level = "warn"
            else:
                why = (
                    "arrival_rtf unavailable (old client) and no server emit_rtf breach — "
                    "cannot separate network jitter from starvation"
                )
                level = "info"
            add("client.jitter", underruns, 0,
                f"{underruns} underrun(s){silence_clause}: {why}",
                level=level, on_critical_path=True, component="transport",
                severity=severity)

    # ── annotators (metadata only, never audio) ──
    lag_budget = targets.get("annotation_lag_budget_ms", 200)
    late_n = int(_num(playback.get("annotation_late_count")) or 0)
    late_max = _num(playback.get("annotation_late_ms_max"))
    client_late = late_n > 0 or (late_max is not None and late_max > 0)
    client_clause = (
        f"; client saw {late_n} annotation(s) arrive after their audio started "
        f"(max {late_max:.0f}ms late)"
        if client_late and late_max is not None else
        (f"; client saw {late_n} annotation(s) arrive after their audio started" if client_late else "")
    )
    for name, an in annotators.items():
        an = an or {}
        comp = f"annotator:{name}"
        if an.get("breaker_open"):
            add("annotator.unavailable", None, None,
                f"annotator {name}: circuit breaker open — no annotations delivered "
                "for this turn (metadata only, audio unaffected)",
                level="error", component=comp, severity="severe")
        lag = _num(an.get("lag_max_ms"))
        if (lag is not None and lag > lag_budget) or client_late:
            if lag is not None and lag > lag_budget:
                add("annotator.lagging", round(lag, 1), lag_budget,
                    f"annotator {name}: annotations lag the audio by up to {lag:.0f}ms "
                    f"(budget {lag_budget:.0f}ms = client prebuffer){client_clause} "
                    "— metadata only, audio unaffected",
                    level="warn", component=comp)
            else:
                add("annotator.lagging", late_max if late_max is not None else late_n, 0,
                    f"annotator {name}: server-side lag within budget{client_clause} "
                    "— metadata only, audio unaffected",
                    level="warn", component=comp, severity="moderate")
        timeouts = int(_num(an.get("timeouts")) or 0)
        dropped = int(_num(an.get("dropped_chunks")) or 0)
        if timeouts > 0 or dropped > 0:
            add("annotator.degraded", timeouts + dropped, 0,
                f"annotator {name}: {timeouts} timeout(s), {dropped} dropped chunk(s) "
                "— some segments unannotated (metadata only, audio unaffected)",
                level="warn", component=comp, severity="moderate")
    if not annotators and client_late:
        # The client reported late annotations but the snapshot has no
        # annotator component (older worker) — still surface it, unnamed.
        add("annotator.lagging", late_max if late_max is not None else late_n, 0,
            f"annotations arrived late{client_clause} — metadata only, audio unaffected",
            level="warn", component="annotator", severity="moderate")

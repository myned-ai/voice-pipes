"""Tests for the client-side tool activity aggregation: `client_tool_call` /
`client_tool_result` trace events pair into the session-detail
`client_tool_calls` list."""

from app.captures.reader import _attribute_calls_to_turns, _collect_client_tool_calls


def _ts(ms: float) -> dict:
    return {
        "secs_since_epoch": int(ms // 1000),
        "nanos_since_epoch": int((ms % 1000) * 1_000_000),
    }


def _evt_ts_ms(evt: dict) -> float | None:
    ts = evt.get("timestamp") or {}
    if isinstance(ts, dict):
        secs = ts.get("secs_since_epoch")
        nanos = ts.get("nanos_since_epoch") or 0
        if isinstance(secs, (int, float)):
            return float(secs) * 1000.0 + float(nanos) / 1_000_000.0
    return None


def _call(call_id, name, args, speak_on_result=False, ts=1000.0):
    return {
        "event_type": "client_tool_call",
        "component": "client_tools",
        "timestamp": _ts(ts),
        "payload": {
            "call_id": call_id,
            "name": name,
            "arguments": args,
            "speak_on_result": speak_on_result,
        },
    }


def _result(
    call_id, phase, result=None, spoken=False, speak_requested=False, name=None, ts=2000.0
):
    return {
        "event_type": "client_tool_result",
        "component": "client_tools",
        "timestamp": _ts(ts),
        "payload": {
            "call_id": call_id,
            "name": name,
            "phase": phase,
            "result": result,
            "spoken": spoken,
            "speak_requested": speak_requested,
        },
    }


def test_call_and_result_pair_into_one_row():
    events = [
        _call("c1", "navigate", {"screen": "settings"}, ts=1000.0),
        _result("c1", "completed", {"ok": True}, spoken=False, ts=2500.0),
    ]
    rows = _collect_client_tool_calls(events, _evt_ts_ms)
    assert len(rows) == 1
    row = rows[0]
    assert row["call_id"] == "c1"
    assert row["name"] == "navigate"
    assert row["arguments"] == {"screen": "settings"}
    assert row["speak_on_result"] is False
    assert row["status"] == "completed"
    assert row["result"] == {"ok": True}
    assert row["spoken"] is False
    assert row["called_at_ms"] == 1000.0
    assert row["resolved_at_ms"] == 2500.0


def test_unresolved_call_stays_pending():
    rows = _collect_client_tool_calls(
        [_call("c1", "navigate", {}, ts=1000.0)], _evt_ts_ms
    )
    assert rows[0]["status"] == "pending"
    assert "result" not in rows[0]


def test_interim_phase_records_first_ack_without_resolving():
    events = [
        _call("c1", "play_media", {}, ts=1000.0),
        _result("c1", "started", ts=1180.0),
        _result("c1", "progress", ts=2000.0),   # later phases don't move the ack
        _result("c1", "completed", {"ok": True}, ts=9000.0),
    ]
    rows = _collect_client_tool_calls(events, _evt_ts_ms)
    row = rows[0]
    assert row["acked_at_ms"] == 1180.0, "first started/progress wins"
    assert row["status"] == "completed"
    assert row["resolved_at_ms"] == 9000.0


def test_first_terminal_result_wins():
    events = [
        _call("c1", "navigate", {}),
        _result("c1", "completed", {"ok": True}, ts=2000.0),
        _result("c1", "failed", {"err": "late duplicate"}, ts=3000.0),
    ]
    rows = _collect_client_tool_calls(events, _evt_ts_ms)
    assert rows[0]["status"] == "completed"
    assert rows[0]["result"] == {"ok": True}


def test_failed_and_spoken_flags_surface():
    events = [
        _call("c1", "play_media", {"title": "jazz"}, speak_on_result=True),
        _result("c1", "failed", {"error": "not found"}, spoken=True, ts=2000.0),
    ]
    rows = _collect_client_tool_calls(events, _evt_ts_ms)
    assert rows[0]["status"] == "failed"
    assert rows[0]["spoken"] is True
    assert rows[0]["speak_on_result"] is True
    # Spoken by tool policy, NOT by client request — the UI attributes
    # the speech accordingly.
    assert rows[0]["speak_requested"] is False


def test_speak_requested_by_client_surfaces():
    events = [
        _call("c1", "navigate", {}),
        _result("c1", "completed", spoken=True, speak_requested=True, ts=2000.0),
    ]
    rows = _collect_client_tool_calls(events, _evt_ts_ms)
    assert rows[0]["spoken"] is True
    assert rows[0]["speak_requested"] is True


def test_orphan_result_creates_row_with_name_fallback():
    # A result whose call event was lost (e.g. truncated capture) still
    # shows up, with the name taken from the result payload.
    rows = _collect_client_tool_calls(
        [_result("c9", "completed", {"ok": 1}, name="highlight", ts=2000.0)],
        _evt_ts_ms,
    )
    assert rows[0]["name"] == "highlight"
    assert rows[0]["status"] == "completed"


def test_attribution_places_calls_between_turns():
    # Turn 1 output at t=10s, turn 2 output at t=30s (epoch ms wallclocks).
    utterances = [
        {"utterance_id": 1, "_wallclock_ts_ms": {"first_audio_out_ms": 10_000.0}},
        {"utterance_id": 2, "_wallclock_ts_ms": {"user_audio_end_ms": 30_000.0}},
        # No wallclock at all — must be skipped, not break ordering.
        {"utterance_id": 3},
    ]
    calls = [
        {"call_id": "before", "called_at_ms": 5_000.0},
        {"call_id": "mid", "called_at_ms": 20_000.0},
        {"call_id": "after", "called_at_ms": 40_000.0},
        {"call_id": "unplaced", "called_at_ms": None},
    ]
    _attribute_calls_to_turns(calls, utterances)
    assert calls[0]["after_utterance_id"] is None, "before turn 1's output"
    assert calls[1]["after_utterance_id"] == 1, "between turn 1 and turn 2"
    assert calls[2]["after_utterance_id"] == 2, "after the last anchored turn"
    assert calls[3]["after_utterance_id"] is None, "no timestamp -> unplaceable"


def test_worker_named_turn_wins_over_timestamps():
    # Gemini 3.x: the call is the model's whole answer, so its turn has no
    # audio and no wallclock anchor. The worker names the turn instead.
    evt = _call("c1", "play_media", {"title": "x"}, ts=20_000.0)
    evt["payload"]["utterance_id"] = 2
    rows = _collect_client_tool_calls([evt], _evt_ts_ms)
    assert rows[0]["utterance_id"] == 2
    utterances = [
        {"utterance_id": 1, "_wallclock_ts_ms": {"first_audio_out_ms": 10_000.0}},
        {"utterance_id": 2},
    ]
    _attribute_calls_to_turns(rows, utterances)
    assert rows[0]["after_utterance_id"] == 2, "not turn 1, the last one with audio"


def test_rows_keep_first_seen_order_and_ignore_other_events():
    events = [
        {"event_type": "llm_detail", "payload": {"elapsed_ms": 1}},
        _call("c2", "b", {}, ts=1000.0),
        _call("c1", "a", {}, ts=1100.0),
        _result("c1", "completed", ts=1200.0),
    ]
    rows = _collect_client_tool_calls(events, _evt_ts_ms)
    assert [r["call_id"] for r in rows] == ["c2", "c1"]

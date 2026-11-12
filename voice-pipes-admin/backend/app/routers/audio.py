"""Capture audio routes.

Every handler hands its work to a worker thread: resolving the session
directory, globbing fragments, reading and concatenating WAVs and finding
speech bounds are all blocking file I/O, and run on the event loop they
stall every other request (the liveness probe included) for as long as
the slowest disk read takes.
"""
import asyncio
import io
import json
import wave as _wave
from pathlib import Path

from fastapi import APIRouter, HTTPException, Query, Request
from fastapi.responses import FileResponse, Response

from ..captures.reader import _resolve_session_dir
from ..captures.speech_bounds import speech_bounds

router = APIRouter()


def _extract_tenant(request: Request | None, tenant_id: str | None) -> str | None:
    if isinstance(tenant_id, str):
        return tenant_id
    if request is not None and hasattr(request, "headers"):
        return request.headers.get("x-tenant-id")
    return None


def _resolve_session_dir_compat(session_id: str, tenant_id: str | None) -> Path:
    if isinstance(tenant_id, str):
        return _resolve_session_dir(session_id, tenant_id=tenant_id)
    return _resolve_session_dir(session_id)


def _turn_frags(session_dir, utterance_id: int):
    """Sorted `{utt:03}_tts_frag_*.wav` paths for a turn (ideal PCM)."""
    prefix = f"{utterance_id:03d}_tts_frag_"
    return sorted(
        session_dir.glob(f"{prefix}*.wav"),
        key=lambda p: int(p.stem.rsplit("_", 1)[-1]),
    )


def _open_wav(path):
    """Open a capture WAV for reading, turning a truncated or malformed
    file (a session cut off mid-write, a disk that filled up) into a 404
    instead of a 500."""
    try:
        return _wave.open(str(path), "rb")
    except (_wave.Error, EOFError, OSError) as exc:
        raise HTTPException(status_code=404, detail="Capture audio unreadable") from exc


def _concat_frag_frames(frags):
    """Concatenate fragment PCM. Returns (nchannels, sampwidth, framerate, frames-bytes).

    Fragments whose format diverges from the first are skipped (defensive —
    TTS providers don't switch mid-turn but a corrupted capture shouldn't
    blow up the whole turn playback). An unreadable fragment is skipped the
    same way; a turn with no readable fragment at all is a 404.
    """
    params = None
    frames = bytearray()
    for fpath in frags:
        try:
            with _wave.open(str(fpath), "rb") as r:
                fmt = (r.getnchannels(), r.getsampwidth(), r.getframerate())
                if params is None:
                    params = fmt
                elif fmt != params:
                    continue
                frames += r.readframes(r.getnframes())
        except (_wave.Error, EOFError, OSError):
            continue
    if params is None:
        raise HTTPException(status_code=404, detail="Capture audio unreadable")
    return params, bytes(frames)


def _wav_response(params, frames: bytes) -> Response:
    nch, width, rate = params
    buf = io.BytesIO()
    with _wave.open(buf, "wb") as w:
        w.setnchannels(nch)
        w.setsampwidth(width)
        w.setframerate(rate)
        w.writeframes(frames)
    return Response(content=buf.getvalue(), media_type="audio/wav")



# ── Session Audio (local captures) ───────────────────────────

@router.get("/api/v1/sessions/{session_id}/audio/turn/{utterance_id}/tts_concat.wav")
async def get_turn_tts_concat(
    session_id: str,
    utterance_id: int,
    request: Request = None,
    tenant_id: str | None = Query(None),
):
    """Stream all `{utt:03}_tts_frag_*.wav` chunks for a turn as a single
    contiguous WAV.

    Realtime providers (OpenAI Realtime / Gemini Live) emit one
    `tts_frag_*` per `response.audio.delta`, which can be 50–200 ms each.
    Native HTMLAudio gaplessly chains *file* loads poorly — each src
    swap triggers a fresh load+decode, producing the click/crack the
    user hears when scrubbing through the admin's session detail.
    Concatenating PCM frames server-side hands the browser one buffer
    that decodes & plays without gaps. Single-fragment turns short-
    circuit to a `FileResponse`, so big-chunk (classic TTS) sessions
    pay zero extra cost.

    NOTE: This route MUST be declared before the catch-all
    `/audio/{filename:path}` below — FastAPI matches routes in
    declaration order and the `:path` converter would otherwise
    swallow `turn/{utt}/tts_concat.wav` and 404.
    """
    eff_tenant = _extract_tenant(request, tenant_id)
    return await asyncio.to_thread(_turn_tts_concat, session_id, utterance_id, eff_tenant)


def _turn_tts_concat(
    session_id: str, utterance_id: int, tenant_id: str | None = None
) -> Response:
    session_dir = _resolve_session_dir_compat(session_id, tenant_id)
    frags = _turn_frags(session_dir, utterance_id)
    if not frags:
        raise HTTPException(status_code=404, detail="No TTS fragments for turn")
    if len(frags) == 1:
        return FileResponse(path=str(frags[0]), media_type="audio/wav")

    params, frames = _concat_frag_frames(frags)
    return _wav_response(params, frames)


@router.get("/api/v1/sessions/{session_id}/audio/turn/{utterance_id}/as-heard.wav")
async def get_turn_as_heard(
    session_id: str,
    utterance_id: int,
    request: Request = None,
    tenant_id: str | None = Query(None),
):
    """Reconstruct the audio the CLIENT actually heard for a turn.

    The ideal fragment concat with the client-reported underrun silences
    spliced back in, from `NNN_playback.json` (gap list in ideal-audio
    coordinates). Without a playback report this degrades gracefully to the
    plain concat (== tts_concat.wav). This is a *reconstruction from client
    playback timing*, not a recording — device/decoder artifacts are not
    represented.

    NOTE: declared before the `/audio/{filename:path}` catch-all — FastAPI
    matches in declaration order.
    """
    eff_tenant = _extract_tenant(request, tenant_id)
    return await asyncio.to_thread(_turn_as_heard, session_id, utterance_id, eff_tenant)


def _turn_as_heard(
    session_id: str, utterance_id: int, tenant_id: str | None = None
) -> Response:
    session_dir = _resolve_session_dir_compat(session_id, tenant_id)
    frags = _turn_frags(session_dir, utterance_id)
    if not frags:
        raise HTTPException(status_code=404, detail="No TTS fragments for turn")

    params, ideal = _concat_frag_frames(frags)
    nch, width, rate = params
    frame_bytes = nch * width
    total_frames = len(ideal) // frame_bytes

    health = None
    health_path = session_dir / f"{utterance_id:03d}_playback.json"
    if health_path.exists():
        try:
            with open(health_path, encoding="utf-8") as fh:
                health = json.load(fh)
        except (OSError, ValueError):
            # Unreadable or torn report: play the server track as is.
            health = None
    gaps = (health or {}).get("gaps") or []
    if not gaps and health is None:
        # No client report — as-heard is indistinguishable from the server track.
        return _wav_response(params, ideal)

    # Interrupted turns: never include ideal audio past what the client heard.
    # `heard_ms` is enqueue-bounded so it is a safe clamp; a resolved
    # `interrupted` flag gates it so a merely-lossy report can't truncate a
    # completed turn.
    resolved_path = session_dir / f"{utterance_id:03d}_turn_resolved.json"
    interrupted = False
    if resolved_path.exists():
        try:
            with open(resolved_path, encoding="utf-8") as fh:
                interrupted = bool(json.load(fh).get("interrupted"))
        except (OSError, ValueError, AttributeError):
            # Unreadable or not an object: treat the turn as completed, so
            # nothing is clamped.
            pass
    if interrupted and health and health.get("heard_ms") is not None:
        clamp_frames = min(total_frames, round(health["heard_ms"] / 1000 * rate))
        ideal = ideal[: clamp_frames * frame_bytes]
        total_frames = clamp_frames

    out = bytearray()
    cursor = 0  # frames
    for g in sorted(gaps, key=lambda x: x.get("at_audio_ms", 0)):
        at = min(total_frames, round((g.get("at_audio_ms") or 0) / 1000 * rate))
        at = max(at, cursor)  # gaps are ascending; never move backwards
        out += ideal[cursor * frame_bytes : at * frame_bytes]
        out += b"\x00" * (round((g.get("silence_ms") or 0) / 1000 * rate) * frame_bytes)
        cursor = at
    out += ideal[cursor * frame_bytes :]
    return _wav_response(params, bytes(out))


@router.get("/api/v1/sessions/{session_id}/audio/turn/{utterance_id}/user_speech.wav")
async def get_turn_user_speech(
    session_id: str,
    utterance_id: int,
    request: Request = None,
    tenant_id: str | None = Query(None),
):
    """The user's mic capture for a turn, cut down to the region that carries
    speech (see `captures/speech_bounds.py`).

    Realtime sessions keep the mic open, so the raw clip is mostly the silence
    around the user's words — including the whole agent reply. Conversation
    Playback plays this instead so the pauses it renders are the real
    response latencies, not capture artefacts. A clip with no speech at all
    is a 404: the playback skips those turns rather than playing silence.

    NOTE: declared before the `/audio/{filename:path}` catch-all — FastAPI
    matches in declaration order.
    """
    eff_tenant = _extract_tenant(request, tenant_id)
    return await asyncio.to_thread(_turn_user_speech, session_id, utterance_id, eff_tenant)


def _turn_user_speech(
    session_id: str, utterance_id: int, tenant_id: str | None = None
) -> Response:
    session_dir = _resolve_session_dir_compat(session_id, tenant_id)
    mic = next(
        (p for k in ("mic_raw", "mic_16k") for p in [session_dir / f"{utterance_id:03d}_{k}.wav"] if p.exists()),
        None,
    )
    if mic is None:
        raise HTTPException(status_code=404, detail="No mic capture for turn")
    bounds = speech_bounds(mic)
    if bounds is None:
        raise HTTPException(status_code=404, detail="No speech in mic capture")

    with _open_wav(mic) as r:
        params = (r.getnchannels(), r.getsampwidth(), r.getframerate())
        rate = r.getframerate()
        start = min(r.getnframes(), round(bounds.start_ms / 1000 * rate))
        end = min(r.getnframes(), round(bounds.end_ms / 1000 * rate))
        r.setpos(start)
        frames = r.readframes(max(0, end - start))
    return _wav_response(params, frames)


@router.get("/api/v1/sessions/{session_id}/audio/{filename:path}")
async def get_session_audio(
    session_id: str,
    filename: str,
    request: Request = None,
    tenant_id: str | None = Query(None),
):
    """Serve any WAV file from a session's capture directory."""
    eff_tenant = _extract_tenant(request, tenant_id)
    return await asyncio.to_thread(_session_audio, session_id, filename, eff_tenant)


def _session_audio(
    session_id: str, filename: str, tenant_id: str | None = None
) -> FileResponse:
    session_dir = _resolve_session_dir_compat(session_id, tenant_id)

    # Allow shorthand "in" / "out" for full-session audio
    if filename == "in":
        filename = "audio_in.wav"
    elif filename == "out":
        filename = "audio_out.wav"

    file_path = session_dir / filename
    # Prevent directory traversal
    if not file_path.resolve().is_relative_to(session_dir.resolve()):
        raise HTTPException(status_code=400, detail="Invalid path")

    if not file_path.exists():
        # Try matching by glob as fallback (e.g. "mic_raw" -> first *_mic_raw.wav).
        # A dropped utterance's recording (`NNN_dropped_<reason>_mic_raw.wav`)
        # is not a turn's audio and must never stand in for one.
        candidates = [p for p in session_dir.glob(f"*_{filename}*.wav") if "_dropped_" not in p.name]
        if candidates:
            file_path = candidates[0]
        else:
            raise HTTPException(status_code=404, detail="Audio file not found")

    return FileResponse(path=str(file_path), media_type="audio/wav")

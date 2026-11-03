"""
VibeVoice-Realtime-0.5B  ─  OpenAI-compatible TTS server.

Serves Microsoft's VibeVoice-Realtime-0.5B model as a local
``/v1/audio/speech`` endpoint, compatible with any client that
speaks the OpenAI TTS API (including voice-pipes' ``openai`` TTS
provider).

Usage:
    python server.py [--port 8091] [--model-path models/vibevoice]
"""

import argparse
import copy
import io
import struct
import sys
import threading
import time
import traceback
from pathlib import Path
from typing import Iterator

import numpy as np
import soundfile as sf
import torch
import uvicorn
from fastapi import FastAPI, HTTPException
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import StreamingResponse
from pydantic import BaseModel
from transformers.cache_utils import DynamicCache
from transformers.modeling_outputs import BaseModelOutputWithPast
from vibevoice import (
    VibeVoiceStreamingForConditionalGenerationInference,
    VibeVoiceStreamingProcessor,
)
from vibevoice.modular.streamer import AudioStreamer

class VoiceMapper:
    """Maps speaker names to voice file paths"""
    
    def __init__(self, voices_dir: Path):
        self.voices_dir = voices_dir
        self.setup_voice_presets()

    def setup_voice_presets(self):
        """Setup voice presets by scanning the voices directory."""
        if not self.voices_dir.exists():
            print(f"Warning: Voices directory not found at {self.voices_dir}")
            self.voice_presets = {}
            self.available_voices = {}
            return
        
        self.voice_presets = {}
        # Get all .pt files in the voices directory
        import glob
        import os
        pt_files = glob.glob(str(self.voices_dir / "**" / "*.pt"), recursive=True)
        
        for pt_file in pt_files:
            # key: filename without extension (and lowercased, and without "en-" prefix etc)
            basename = os.path.splitext(os.path.basename(pt_file))[0]
            name = basename.lower()
            full_path = os.path.abspath(pt_file)
            self.voice_presets[name] = full_path
            
            # Also register name without the language prefix (e.g. "carter" from "en-carter_man")
            if "-" in name:
                parts = name.split("-", 1)
                self.voice_presets[parts[1]] = full_path
                # Also without gender prefix if there is "_"
                if "_" in parts[1]:
                    self.voice_presets[parts[1].split("_", 1)[0]] = full_path
            elif "_" in name:
                self.voice_presets[name.split("_", 1)[0]] = full_path

        # Sort the voice presets
        self.voice_presets = dict(sorted(self.voice_presets.items()))
        self.available_voices = {
            name: path for name, path in self.voice_presets.items()
            if os.path.exists(path)
        }
        
        print(f"Found {len(self.available_voices)} voice aliases in {self.voices_dir}")

    def get_voice_path(self, speaker_name: str) -> str:
        """Get voice file path for a given speaker name"""
        if not self.voice_presets:
            raise ValueError(f"No voice presets found in {self.voices_dir}")
            
        speaker_name = speaker_name.lower()
        if speaker_name in self.voice_presets:
            return self.voice_presets[speaker_name]
        
        # Try partial matching (case insensitive)
        matched_path = None
        for preset_name, path in self.voice_presets.items():
            if preset_name in speaker_name or speaker_name in preset_name:
                matched_path = path
                break
        if matched_path is not None:
            return matched_path
        
        # Default to first voice if no match found
        default_voice = list(self.voice_presets.values())[0]
        print(f"Warning: No voice preset found for '{speaker_name}', using default: {default_voice}")
        return default_voice



# ── CLI ──────────────────────────────────────────────────────

def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="VibeVoice-Realtime TTS server")
    p.add_argument("--port", type=int, default=8091)
    p.add_argument("--host", default="0.0.0.0")
    p.add_argument(
        "--model-path",
        default="microsoft/VibeVoice-Realtime-0.5B",
        help="HuggingFace repo id or local path to the model",
    )
    p.add_argument(
        "--device",
        default="cuda" if torch.cuda.is_available() else "cpu",
    )
    p.add_argument("--dtype", default="float16", choices=["float16", "float32", "bfloat16"])
    p.add_argument(
        "--skip-warmup",
        action="store_true",
        help="Skip the startup warm-up synthesis (first request will run cold)",
    )
    return p.parse_args()


# ── Model loading ────────────────────────────────────────────

_processor = None
_model = None
_voice_mapper = None
_device = "cpu"
_sample_rate = 24_000
# The model (and its diffusion scheduler's `step_index`) is shared and NOT
# thread-safe. Serialize generation so two requests — or a leaked worker from a
# barged-in request and a fresh one — never run `generate()` concurrently and
# race the scheduler (which manifests as `IndexError: index N out of bounds`).
_GEN_LOCK = threading.Lock()
# Identity advertised on /v1/models so a client (e.g. voice-pipes' openai TTS
# provider) can record which model + runtime config actually served a session,
# even though it runs in this separate process.
_model_id = "VibeVoice-Realtime-0.5B"
_model_version = "unknown"
_DDPM_STEPS = 5
# True once the startup warm-up synthesis has completed (surfaced on /health).
_warmed = False


def load_model(model_path: str, device: str, dtype_str: str):
    global _processor, _model, _voice_mapper, _device, _sample_rate
    global _model_id, _model_version

    base_dir = Path(__file__).resolve().parent.parent.parent
    if (base_dir / "voice-pipes").is_dir():
        base_dir = base_dir / "voice-pipes"
    voices_dir = base_dir / "VibeVoice" / "demo" / "voices" / "streaming_model"
    _voice_mapper = VoiceMapper(voices_dir)

    dtype_map = {
        "float16": torch.float16,
        "float32": torch.float32,
        "bfloat16": torch.bfloat16,
    }
    dtype = dtype_map[dtype_str]
    _device = device

    if device == "cpu":
        print("Forcing float32 precision for CPU execution to prevent runtime/performance issues.")
        dtype = torch.float32

    print(f"Loading VibeVoice model from {model_path} on {device} ({dtype_str})...")
    t0 = time.time()

    _processor = VibeVoiceStreamingProcessor.from_pretrained(model_path, trust_remote_code=True)
    
    if device == "cuda":
        attn_impl = "flash_attention_2"
    else:
        attn_impl = "sdpa"

    attn_used = attn_impl
    try:
        _model = VibeVoiceStreamingForConditionalGenerationInference.from_pretrained(
            model_path,
            torch_dtype=dtype,
            device_map=device,
            attn_implementation=attn_impl,
            trust_remote_code=True,
        )
    except Exception as e:
        if attn_impl == 'flash_attention_2':
            print(f"Failed to load with flash_attention_2. Falling back to sdpa. Error: {e}")
            attn_used = 'sdpa'
            _model = VibeVoiceStreamingForConditionalGenerationInference.from_pretrained(
                model_path,
                torch_dtype=dtype,
                device_map=device,
                attn_implementation='sdpa',
                trust_remote_code=True,
            )
        else:
            raise e

    _model.eval()
    _model.set_ddpm_inference_steps(num_steps=_DDPM_STEPS)

    # Runtime descriptor surfaced on /v1/models. There's no upstream "model
    # version" for VibeVoice, so we report the runtime config that actually
    # affects output/perf (dtype, attention impl — flash vs sdpa, ddpm steps,
    # weights source). This is what shows up in the voice-pipes session report.
    _model_id = f"VibeVoice-Realtime-0.5B ({Path(model_path).name})"
    _model_version = f"{dtype_str}/{attn_used}/ddpm{_DDPM_STEPS}/{device}"

    elapsed = time.time() - t0
    vram_str = ""
    if torch.cuda.is_available():
        vram_str = f"  (VRAM: {torch.cuda.memory_allocated() / 1e9:.2f} GB)"
    print(f"Model loaded in {elapsed:.1f}s{vram_str}")


# ── FastAPI ──────────────────────────────────────────────────

app = FastAPI(title="VibeVoice-Realtime TTS", version="1.0.0")


class SpeechRequest(BaseModel):
    """OpenAI-compatible /v1/audio/speech request body."""
    model: str = "VibeVoice-Realtime-0.5B"
    input: str
    voice: str = "default"
    response_format: str = "wav"  # wav | pcm
    speed: float = 1.0


@app.get("/v1/models")
async def list_models():
    """`/v1/models` — advertises the loaded model id + runtime version so a
    client can record what actually served the session (cross-process)."""
    return {
        "object": "list",
        "data": [
            {
                "id": _model_id,
                "object": "model",
                "owned_by": "microsoft",
                "version": _model_version,
            }
        ],
    }


def cast_tensors(obj, dtype):
    """Recursively cast floating point tensors in obj to the target dtype in-place."""
    if isinstance(obj, torch.Tensor):
        if torch.is_floating_point(obj):
            return obj.to(dtype=dtype)
        return obj
    elif isinstance(obj, dict):
        for k, v in list(obj.items()):
            obj[k] = cast_tensors(v, dtype)
        return obj
    elif isinstance(obj, list):
        for i, x in enumerate(obj):
            obj[i] = cast_tensors(x, dtype)
        return obj
    elif isinstance(obj, tuple):
        return tuple(cast_tensors(x, dtype) for x in obj)
    elif hasattr(obj, '__dict__'):
        for k, v in list(obj.__dict__.items()):
            setattr(obj, k, cast_tensors(v, dtype))
        if hasattr(obj, '_key_cache'):
            obj._key_cache = [cast_tensors(x, dtype) for x in obj._key_cache]
        if hasattr(obj, '_value_cache'):
            obj._value_cache = [cast_tensors(x, dtype) for x in obj._value_cache]
        return obj
    return obj


def _prepare_inputs(text: str, voice_name: str):
    """Load the voice prompt and build model inputs on the target device.
    Returns `(inputs, all_prefilled_outputs)`."""
    voice_path = _voice_mapper.get_voice_path(voice_name)
    target_device = _device if _device != "cpu" else "cpu"

    all_prefilled_outputs = torch.load(voice_path, map_location=target_device, weights_only=False)
    all_prefilled_outputs = cast_tensors(all_prefilled_outputs, _model.dtype)

    inputs = _processor.process_input_with_cached_prompt(
        text=text,
        cached_prompt=all_prefilled_outputs,
        padding=True,
        return_tensors="pt",
        return_attention_mask=True,
    )
    for k, v in inputs.items():
        if torch.is_tensor(v):
            inputs[k] = v.to(target_device)
    return inputs, all_prefilled_outputs


def _synthesize_stream(text: str, voice_name: str = "default") -> Iterator[np.ndarray]:
    """Stream float32 audio chunks as VibeVoice generates them.

    `generate()` runs on a background thread and pushes audio chunks into an
    `AudioStreamer` queue; we yield them as they arrive so the HTTP response's
    first byte is the *first* chunk (≈ first-chunk latency) instead of the full
    synthesis time. Mirrors `VibeVoice/demo/web/app.py::stream`.
    """
    inputs, prefilled = _prepare_inputs(text, voice_name)
    audio_streamer = AudioStreamer(batch_size=1, stop_signal=None, timeout=None)
    errors: list = []
    stop_event = threading.Event()

    def _run() -> None:
        try:
            # Serialize all model/scheduler access: only one generation at a
            # time. A barged-in request whose generator we close sets
            # `stop_event`, so its `generate()` exits promptly (via
            # `stop_check_fn`) and releases the lock before the next request.
            with _GEN_LOCK:
                # Reset the diffusion scheduler for THIS generation (under the
                # lock so the reset + run are atomic w.r.t. other requests).
                _model.set_ddpm_inference_steps(num_steps=_DDPM_STEPS)
                with torch.no_grad():
                    _model.generate(
                        **inputs,
                        max_new_tokens=None,
                        cfg_scale=1.5,
                        tokenizer=_processor.tokenizer,
                        generation_config={"do_sample": False},
                        audio_streamer=audio_streamer,
                        stop_check_fn=stop_event.is_set,
                        verbose=False,
                        # Match VibeVoice's official streaming demo exactly
                        # (demo/web/app.py::_run_generation).
                        refresh_negative=True,
                        all_prefilled_outputs=copy.deepcopy(prefilled),
                    )
        except Exception as exc:  # noqa: BLE001 — surfaced to the request below
            errors.append(exc)
            traceback.print_exc()
        finally:
            # Always end the stream so the consumer's get_stream() terminates.
            audio_streamer.end()

    thread = threading.Thread(target=_run, daemon=True)
    thread.start()

    try:
        for chunk in audio_streamer.get_stream(0):
            if torch.is_tensor(chunk):
                chunk = chunk.detach().cpu().to(torch.float32).numpy()
            else:
                chunk = np.asarray(chunk, dtype=np.float32)
            if chunk.ndim > 1:
                chunk = chunk.reshape(-1)
            peak = float(np.max(np.abs(chunk))) if chunk.size else 0.0
            if peak > 1.0:
                chunk = chunk / peak
            yield chunk.astype(np.float32, copy=False)
    finally:
        # Stop generation and reap the worker whether we finished or the client
        # disconnected mid-stream (GeneratorExit).
        stop_event.set()
        audio_streamer.end()
        thread.join()

    if errors:
        raise errors[0]


def warmup_model() -> None:
    """Fire one throwaway synthesis so the first real request doesn't pay the
    one-time first-inference cost: CUDA kernel load/autotune + allocator growth
    on the first forward pass, plus the first voice-prompt ``torch.load`` from a
    cold page cache. Observed as ~8.9s TTFB on the cold turn vs ~1.6s warm
    (session 03cdae60). Runs before uvicorn binds the port, so clients can't
    reach a cold server. Best-effort: a failure must never prevent serving —
    the first real request just runs cold instead."""
    global _warmed
    print("Running warm-up synthesis...")
    t0 = time.time()
    try:
        samples = 0
        for chunk in _synthesize_stream("Warming up the voice pipeline.", "default"):
            samples += chunk.size
        print(f"Warm-up synthesis complete in {time.time() - t0:.1f}s "
              f"({samples / _sample_rate:.1f}s of audio)")
        _warmed = True
    except Exception:  # noqa: BLE001 — warm-up is best-effort by design
        traceback.print_exc()
        print("Warm-up synthesis failed (non-fatal); first request will pay the cold-start cost")


def _to_pcm16(waveform: np.ndarray) -> bytes:
    """Convert float32 [-1,1] waveform to raw PCM 16-bit LE bytes."""
    clipped = np.clip(waveform, -1.0, 1.0)
    pcm = (clipped * 32767).astype(np.int16)
    return pcm.tobytes()


def _to_wav(waveform: np.ndarray, sr: int) -> bytes:
    """Convert float32 waveform to WAV bytes."""
    buf = io.BytesIO()
    sf.write(buf, waveform, sr, format="WAV", subtype="PCM_16")
    buf.seek(0)
    return buf.read()


@app.post("/v1/audio/speech")
async def create_speech(req: SpeechRequest):
    if not req.input or not req.input.strip():
        raise HTTPException(status_code=400, detail="Empty input text")

    # PCM: stream chunks as they're generated. No Content-Length → chunked
    # transfer, so the client receives the first chunk at ~first-chunk latency
    # instead of waiting for the whole clip (the voice-pipes openai TTS client
    # always requests `response_format: "pcm"`).
    if req.response_format == "pcm":
        t0 = time.time()
        gen = _synthesize_stream(req.input, req.voice)
        # Prime the first chunk before returning the streaming response: if
        # generate() fails immediately (the common failure), this raises here
        # and we return a clean HTTP 500 instead of an aborted chunked stream
        # (which the client only sees as "unexpected EOF"). Same TTFB — we wait
        # for the first chunk either way.
        try:
            first_chunk = await run_in_threadpool(lambda: next(gen, None))
        except Exception as e:
            gen.close()
            traceback.print_exc()
            raise HTTPException(status_code=500, detail=f"Synthesis failed: {e}")
        if first_chunk is not None:
            print(
                f"VibeVoice first audio chunk in {(time.time() - t0) * 1000:.0f}ms "
                f"({len(req.input)} chars)"
            )

        def _pcm_stream() -> Iterator[bytes]:
            total_samples = 0
            try:
                if first_chunk is not None:
                    total_samples += int(first_chunk.size)
                    yield _to_pcm16(first_chunk)
                for chunk in gen:
                    total_samples += int(chunk.size)
                    yield _to_pcm16(chunk)
            finally:
                # On client disconnect / barge-in the consumer is closed here;
                # close the underlying generator so its worker thread is stopped
                # and joined (otherwise it keeps generating on the shared model
                # and races the next request — the IndexError cause).
                gen.close()
            dur_s = total_samples / _sample_rate
            el_s = time.time() - t0
            rtfx = dur_s / el_s if el_s > 0 else 0.0
            print(
                f"Synthesized {len(req.input)} chars -> {dur_s:.2f}s audio "
                f"in {el_s * 1000:.0f}ms (RTFx={rtfx:.1f}, streamed)"
            )

        return StreamingResponse(_pcm_stream(), media_type="application/octet-stream")

    # WAV: header needs the total length up front, so buffer the stream.
    try:
        chunks = list(_synthesize_stream(req.input, req.voice))
    except Exception as e:
        traceback.print_exc()
        raise HTTPException(status_code=500, detail=f"Synthesis failed: {e}")
    waveform = np.concatenate(chunks) if chunks else np.zeros(0, dtype=np.float32)
    wav = _to_wav(waveform, _sample_rate)
    return StreamingResponse(
        io.BytesIO(wav),
        media_type="audio/wav",
        headers={"Content-Length": str(len(wav))},
    )


@app.get("/health")
async def health():
    return {"status": "ok", "model_loaded": _model is not None, "warmed": _warmed}


# ── Entrypoint ───────────────────────────────────────────────

if __name__ == "__main__":
    args = parse_args()
    load_model(args.model_path, args.device, args.dtype)
    if not args.skip_warmup:
        warmup_model()
    print(f"Starting server on {args.host}:{args.port}")
    uvicorn.run(app, host=args.host, port=args.port, log_level="info")

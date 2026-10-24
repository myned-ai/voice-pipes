# TTS Streaming Latency & Buffer Optimization

**Status:** active
**Last verified:** 2026-09-06 (moved into the public tree; content unchanged)
**Owner:** Docs

## Overview
During the integration and benchmarking of Kyutai's Pocket-TTS, we observed unexpectedly high Time-To-First-Byte (TTFB) latencies. Despite the model being capable of yielding its first audio chunk in ~200-300ms, the end-to-end pipeline measured TTFBs ranging from 800ms up to 2.8 seconds.

This document outlines the root causes of this latency inflation and the implemented solutions, specifically the "Silence Padding" technique to bypass chunk mismatch buffering.

## The Root Causes

### 1. The Chunk Size Mismatch (Buffer Starvation)
The primary cause of the artificial latency was a hard mismatch between the chunk sizes emitted by the PyTorch model and the minimum chunk size expected by the Rust streaming client.

- **Pocket-TTS Output:** The PyTorch `generate_audio_stream` function natively yields audio tensors of `1920 samples` (80ms at 24kHz).
- **Voice-Pipes Rust Client:** The `pocket-http` Rust client defines a hardcoded constant `STREAM_CHUNK_SAMPLES = 2400` (100ms) and buffers all incoming network bytes until this threshold is met.

**The Bottleneck:**
When the first 1920-sample chunk arrives over the network (which takes ~269ms of inference time), the Rust client buffers it because `1920 < 2400`. The pipeline refuses to flush the audio to the frontend, forcing it to wait in silence for the *second* chunk to be computed and delivered. This strictly forces the TTFB to double, as the pipeline blocks until at least two inference cycles have completed.

### 2. Synchronous Generator Threading in FastAPI
The initial HTTP streaming server used a synchronous generator for PyTorch execution within FastAPI's `StreamingResponse`. While Starlette runs sync generators in a background threadpool, consecutive or overlapping TTS requests (e.g., when the LLM emits multiple sentences in rapid succession) can cause PyTorch CPU thread contention. This thrashing blocked the event loop and caused massive TTFB spikes (up to 2.7s) on longer sentences.

## The Solution: Silence Padding & Async Yielding

To resolve these issues and hit the strict `<400ms` TTFB target, we deployed a custom streaming proxy (`scripts/pocket-tts/serve_pocket_streaming.py`) with two major architectural tweaks:

### 1. The Silence Padding Trick
To bypass the Rust client's buffer starvation, the streaming server now immediately yields exactly `2400 samples` of pure silence (`0x00` int16 PCM bytes) the millisecond the HTTP connection is established, immediately following the WAV header.

**Why this works:**
1. The 2400 samples of silence instantly fill the Rust client's `STREAM_CHUNK_SAMPLES` buffer.
2. The Rust client immediately flushes this chunk down the pipeline, dropping the network-level TTFB to practically 0ms.
3. The frontend begins "playing" the 100ms of silence. This 100ms is imperceptible to the user.
4. While the silence plays, PyTorch is asynchronously computing the actual first chunk of audio. By the time the silence finishes, the first real audio chunk has arrived and plays seamlessly.

### 2. Async `to_thread` Delegation
The generator was rewritten as an `async def` function, explicitly delegating the blocking `next(gen)` PyTorch calls to `asyncio.to_thread()`. This guarantees that heavy FP32/INT8 math operations do not block the ASGI event loop or starve the threadpool, allowing concurrent streams to execute safely.

## Implementation Reference

The optimizations are implemented in the `scripts/pocket-tts/serve_pocket_streaming.py` wrapper:

```python
async def audio_generator():
    yield make_wav_header()
    
    # 1. PADDING TRICK: Yield 2400 samples (100ms) of silence
    silence = np.zeros(2400, dtype=np.int16)
    yield silence.tobytes()
    
    # 2. Async generation
    import asyncio
    gen = model.generate_audio_stream(voice_state, text)
    while True:
        try:
            chunk = await asyncio.to_thread(next, gen)
            # ... format conversion ...
            yield arr.tobytes()
        except StopIteration:
            break
```

## Results
With these initial fixes, TTFB metrics for `pocket-http` dropped to ~250-300ms.

### 3. Orchestrator Sentence Buffering (The Final Latency Fix)
Despite fixing the Python HTTP server, the end-to-end latency was still artificially capped by the Rust Orchestrator itself.

The `tts_task` pipeline explicitly buffered all audio frames for an entire text fragment (usually a full sentence) before it forwarded *any* audio to the client. This blocking behavior was required to calculate the exact `total_fragment_duration_ms` up front so that the frontend's Karaoke UI could sync correctly.

**The Solution: Zero-Delay Streaming & Metadata Updates**
To achieve absolute minimum TTFB, the sentence-level buffer in `tts_task` was eliminated, allowing audio to stream to the frontend instantly as it arrives from the PyTorch model (or gRPC stream).

To maintain Karaoke UI synchronization without the upfront duration:
1. The orchestrator attaches a `CaptionMeta` with an unknown duration to the very first audio chunk of a sentence.
2. Once the sentence completes, the orchestrator calculates the true duration and emits a final, 0-sample trailing `AudioChunk` containing the updated `CaptionMeta`.
3. The frontend (`VoicePipesSessionCore` and `WordTimingTable`) intercepts this 0-sample chunk and retroactively stretches the active fragment duration (`stretchLastFragmentTo`) without duplicating the text on the screen.

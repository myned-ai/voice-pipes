# Pocket-TTS (TTS, local sidecar)

**Status:** active
**Last verified:** 2026-09-27
**Owner:** Docs

## What it is

Kyutai's Pocket-TTS served by its own `pocket-tts serve` HTTP server. The
sidecar owns the model files, voice catalogue and inference; Voice Pipes
speaks its HTTP API. Language variants exist beyond English.

It is a plugin crate, `voice-pipes-tts-pocket`, built on `voice-pipes-sdk`
and `voice-pipes-net`; the engine contains no Pocket-TTS code. The voice is a
live parameter: a change from the admin console applies on the next
synthesis, as before.

## Cargo feature

`tts-pocket` on the **CLI crate**:
`cargo build -p voice-pipes-cli --features tts-pocket`. Both worker
images carry it. (Until 2026-09 it was an engine feature; `-p voice-pipes
--features tts-pocket` no longer exists.)

## Config

```toml
[tts]
provider = "pocket-http"
endpoint = "http://127.0.0.1:8000"   # in the Docker stack: http://pocket-tts:8000
voice = "alba"                       # a built-in name, or "hf://kyutai/tts-voices/<path>.wav"

[tts.options]
# request_timeout_ms = 60000
# allow_voice_urls = false   # let a per-turn voice *hint* (tool result, client tool)
#                            # name an http(s):// or hf:// voice the sidecar would
#                            # download; the configured `voice` above is always allowed
```

## Running the sidecar

```sh
# native, needs uv
uvx pocket-tts generate --text "warm" --output-path warm.wav    # one-time model cache
uvx pocket-tts serve --host 127.0.0.1 --port 8000
uvx pocket-tts serve --language french_24l --port 8000          # a non-English variant

# Docker
docker run --rm -d --name pocket-tts -p 127.0.0.1:8000:8000 \
  -v pocket-tts-cache:/root/.cache/huggingface \
  ghcr.io/kyutai-labs/pocket-tts:latest serve --host 0.0.0.0 --port 8000
```

The Docker launcher starts it as the `pocket-tts` compose service when you
pick it in the wizard. There is no model file under `models/`; the sidecar
caches package installations and weights under `models/.cache/pocket-tts/`.

The compose service includes a container readiness `healthcheck` probing
`http://127.0.0.1:8000/health`. On startup, its entrypoint runs a warm-up synthesis
to download and resident the weights in memory; the fleet launcher scripts
(`launch-stack.ps1` and `launch-stack.sh`) poll this probe and wait until the sidecar
reports `healthy` before declaring backend readiness.

## Environment

None.

## Capabilities

| | |
|---|---|
| Streaming | Yes, chunked HTTP |
| Word timestamps | No |
| Voices | built-in names and `hf://` voice samples |

## Limitations

- The model emits small audio chunks; without care the client's minimum chunk
  size starved playback and inflated time to first byte from ~250 ms to
  seconds. The provider pads the first chunk with silence to release it
  immediately; the story is in [TTS streaming latency](../../../explanation/tts-streaming-latency.md).
- The `*_24l` multilingual variants are larger and slower preview models.
- A gRPC wrapper (`scripts/pocket-tts/serve_pocket_grpc.py`) also exists for
  dynamic voice selection from the admin console; it is a development
  utility, not the supported path.

## Upstream

`github.com/kyutai-labs/pocket-tts`.

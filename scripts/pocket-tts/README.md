# Pocket-TTS sidecar prototypes

Standalone wrappers around [Kyutai Pocket-TTS](https://github.com/kyutai-labs)
that expose it to the voice-pipes engine. These predate the supported Docker
sidecar (`pocket-tts` compose profile, which runs `uvx pocket-tts serve` — see
`voice-pipes/deploy/docker/docker-compose.yml`) and remain useful for running
the model on the host directly or for latency experiments.

| Script | What it does |
|---|---|
| `serve_pocket_grpc.py` | gRPC TTS provider (`voice-pipes/proto/tts.proto` + `params.proto`) on `:50051`. Streams discrete PCM frames over HTTP/2 — avoids the TTFB inflation of HTTP chunk buffering. Protos are compiled at import time; no checked-in gencode. |
| `serve_pocket_streaming.py` | FastAPI streaming HTTP server (`POST /tts`, chunked WAV) with a padding trick that flushes the first client buffer immediately. |
| `benchmark_pocket.py` | Raw synthesis RTF/TTFB microbenchmark, no transport overhead. |
| `test_client.py` | Smoke client for the gRPC server (run `serve_pocket_grpc.py` first — it generates the `*_pb2` modules this imports). |

## Setup

```sh
pip install -r requirements.txt
python serve_pocket_grpc.py          # or serve_pocket_streaming.py --port 8000
```

Or without installing anything:

```sh
uv run --with pocket-tts --with grpcio --with grpcio-tools python serve_pocket_grpc.py
```

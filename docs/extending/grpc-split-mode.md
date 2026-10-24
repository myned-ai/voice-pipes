# gRPC split mode

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

Any pipeline stage can run as a separate process, on another machine or GPU,
and be consumed by a worker over gRPC. This is how you keep ONNX or CUDA
inference in a dedicated process, scale one hot stage independently, or mix
local and remote stages.

## The two halves

| Side | Feature | What it does |
|---|---|---|
| Component server | `component-server` | The `voice-pipes-component` binary loads a provider from the same TOML format and serves it as a gRPC service |
| Worker client | `stt-grpc`, `tts-grpc`, `text-grpc` | `provider = "grpc"` / `type = "grpc"` with an `endpoint` connects to a component server |

Protos live in `voice-pipes/proto/`: `stt.proto` (`SttService`), `tts.proto`
(`TtsService`), `text_processor.proto` (`TextProcessorService`). All three
are streaming RPCs; `tonic-build` compiles them at build time with the
vendored `protoc`.

## Example: remote Piper TTS

Component server (any machine with the Piper model):

```toml
# worker-tts.toml
[tts]
provider = "piper"
model = "models/en_US-lessac-medium.onnx"
```

```sh
cargo run --features component-server,tts-piper --bin voice-pipes-component -- \
  --config worker-tts.toml --listen 0.0.0.0:50051
```

Worker (no Piper model, no eSpeak NG):

```toml
# pipeline-remote-tts.toml
[vad]
mode = "silero"
model = "models/silero_vad.onnx"

[stt]
provider = "parakeet-tdt"

[tts]
provider = "grpc"
endpoint = "http://tts-host:50051"
```

```sh
cargo run --features stt-grpc,tts-grpc,text-grpc -- --config pipeline-remote-tts.toml live
```

The same shape works for STT (`[stt] provider = "grpc"`) and for text
processors (`[[text_processor]] type = "grpc"`). The gRPC clients take
only their endpoint; the component server decides everything else.

## Topologies this enables

- **GPU isolation.** One CUDA process serving STT to several CPU workers.
- **Horizontal scaling.** Several TTS component servers behind a load
  balancer; workers point at the balancer.
- **Mixed deployments.** Local STT, remote GPU LLM, cloud TTS, in one
  pipeline.

## Related protocols

Two more gRPC contracts use the same toolchain but are not split-mode
providers:

- `audio_post_processor.proto`: the `AudioPostProcessor` service with a
  unary `Process` and a streaming `Annotate`, used by
  [audio annotators](../guides/audio-annotators.md).
- `agent.proto`: the `AgentService` an external agent implements to be the
  conversation's brain or its tool backend; see
  [Tool calling](../guides/tool-calling.md) and the Python
  [reference agent](../../examples/README.md).

## Health and metrics

A gRPC provider is a component like any other: connection failures mark it
unhealthy on the worker's `/health`, and its latency appears in the per-turn
metrics under the stage it serves. The far side's identity is reported when
the component server exposes it.

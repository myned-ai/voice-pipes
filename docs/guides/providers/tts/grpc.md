# gRPC TTS client

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

## What it is

A client for the `TtsService` served by a `voice-pipes-component` process,
so any TTS provider can run on another machine or GPU. See
[gRPC split mode](../../../extending/grpc-split-mode.md).

## Cargo feature

`tts-grpc` on the worker; `component-server` plus the provider's feature on
the server.

## Config

```toml
[tts]
provider = "grpc"
endpoint = "http://tts-host:50051"
```

## Environment

None on the worker.

## Capabilities

Bidirectional streaming: text and synthesis hints in, audio frames out.
Voice, language and speed hints are forwarded, so the far side's provider
can honour per-session voices.

## Limitations

Plaintext gRPC intended for a private network. One extra hop per audio
frame.

## Upstream

`voice-pipes/proto/tts.proto`.

# gRPC STT client

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

## What it is

A client for the `SttService` served by a `voice-pipes-component` process.
Any STT provider can be put behind it to run on another machine or GPU. See
[gRPC split mode](../../../extending/grpc-split-mode.md).

## Cargo feature

`stt-grpc` on the worker; `component-server` plus the real provider's
feature on the server.

## Config

```toml
[stt]
provider = "grpc"
endpoint = "http://stt-host:50051"
```

The client has no settings of its own: the component server it connects to
decides the model and the sample rate.

## Environment

None on the worker; the component server needs whatever its provider needs.

## Capabilities

Streaming audio in, `TranscriptionResult` stream out. Capabilities beyond
streaming depend on the provider behind the server.

## Limitations

Plaintext gRPC on the local network; put TLS termination in front for
anything else. Latency adds one network hop per audio chunk.

## Upstream

`voice-pipes/proto/stt.proto`.

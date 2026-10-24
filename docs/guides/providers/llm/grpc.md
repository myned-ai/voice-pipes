# gRPC text processor client

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

## What it is

A client for the `TextProcessorService` served by a `voice-pipes-component`
process, so an LLM or any text processor can run elsewhere. See
[gRPC split mode](../../../extending/grpc-split-mode.md).

## Cargo feature

`text-grpc` on the worker; `component-server` plus the processor's feature
on the server.

## Config

```toml
[[text_processor]]
type = "grpc"
endpoint = "http://llm-host:50052"
```

## Capabilities

Streaming text fragments in both directions; sentence chunking happens on
the worker.

## Limitations

Plaintext gRPC for a private network.

## Upstream

`voice-pipes/proto/text_processor.proto`.

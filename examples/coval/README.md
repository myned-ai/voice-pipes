# Running Coval AI Simulations Against Voice Pipes

This directory is a **config-only** integration recipe (removable via `rm -rf examples/coval`). No Coval-specific code exists in `voice-pipes` core.

## 1. Set Environment Variables

```sh
# Required for fail-closed `/ws/raw` authentication
export VP_CLIENT_TOKEN="replace-with-strong-random-token"

# Standard OpenTelemetry OTLP/HTTP export to Coval (TLS enforced)
export OTEL_EXPORTER_OTLP_PROTOCOL="http/protobuf"
export OTEL_EXPORTER_OTLP_ENDPOINT="https://api.coval.dev/v1/traces"
export OTEL_EXPORTER_OTLP_HEADERS="X-API-Key=your_coval_api_key"

# Optional: if using Coval's Twilio Media Streams mode
export TWILIO_AUTH_TOKEN="your_twilio_auth_token"
```

## 2. Start Voice Pipes

```sh
# Raw PCM WebSocket mode (built-in)
cargo run -p voice-pipes-cli --features telemetry -- \
  --config examples/coval/config.toml serve

# Or with the isolated Twilio Media Streams plugin enabled
cargo run -p voice-pipes-cli --features "telemetry,transport-twilio" -- \
  --config examples/coval/config.toml serve
```

## 3. Connect Coval AI

- **WebSocket Audio Mode:** Point Coval to `wss://<your-host>/ws/raw?sample_rate=16000&encoding=pcm16` with `Authorization: Bearer $VP_CLIENT_TOKEN`. Coval's `X-Coval-Simulation-Id` header is automatically sanitized and attached as `external_id` on the session.
- **Twilio Media Streams Mode:** Point Coval to `https://<your-host>/telephony/twilio/voice` (validates `X-Twilio-Signature` and mints a single-use 30 s stream token for `/telephony/twilio/ws`).

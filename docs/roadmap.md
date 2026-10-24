# Roadmap

**Status:** active
**Last verified:** 2026-10-18
**Owner:** Maintainers

What we intend to build next, in rough priority order. Nothing here is a
commitment with a date. Provider tables in the docs list only what works
today; anything "coming" lives on this page instead.

If you want to work on one of these, open a discussion first so we can agree
on the shape.

## Near term

- **Ephemeral client JWT tokens (AuthN/AuthZ).** Transitioning client ingress
  from static shared bearer tokens (`VP_CLIENT_TOKEN`) to short-lived, signed
  ephemeral JWT session tokens (HMAC-SHA256 / RSA with `iss`, `sub`, `exp`,
  pipeline, and capability claims) minted by application backends. Prevents
  frontend credential exposure in production web and mobile deployments.
- **Admin Console SSO / OIDC & RBAC.** Corporate single sign-on (OAuth2 / OIDC
  via Google, GitHub, Okta, Keycloak) and role-based access control (Admin,
  Operator, QA Reviewer) for the FastAPI admin backend and dashboard.
- **Tenant-isolated capture storage.** Sandboxing audio captures, session logs,
  and SQLite analytics rollups by `tenant_id` with time-limited presigned audio
  delivery URLs for GDPR/HIPAA-compliant multi-tenant deployments.
- **Telephony (PSTN/SIP).** Twilio and Telnyx media streams are WebSocket
  JSON with base64 µ-law; the transport already speaks WebSocket PCM, so this
  is a codec plus a serializer. Includes telephony webhook signature
  verification (`X-Twilio-Signature`).
- **Python client.** A thin async client over the documented
  [wire protocol](clients/protocol.md) for people evaluating from Python-first
  stacks.
- **`@voice-pipes/client` on npm.** The TypeScript session core and pure FSM
  currently live inside the admin frontend; extracting them is planned.
- **`voice-pipes-sdk` on crates.io** so plugin authors can depend on a
  version instead of a git tag.
- **Curated examples directory** with one folder per use case (local-only,
  cloud stack, speech-to-speech, client tools, avatar annotators, Python
  client, gRPC agent).

## Providers

- STT: Deepgram, Azure Speech.
- TTS: further local models as they prove faster than real time on CPU.
- Speech-to-speech: keep pace with provider API changes (model ids are
  configurable, so a rename is a config edit, not a release).

See [the STT provider comparison](explanation/stt-provider-comparison.md) for
the evaluation notes behind the STT choices.

## Later

- **WebRTC and WebTransport.** Today's only client transport is WebSocket.
  The worker's `/wt` route answers `501 Not Implemented`, and the console's
  WebRTC and WebTransport adapters are gated stubs. Needed for browser
  deployments outside a LAN with real NAT traversal.
- **Opus on the wire.** Binary frame type `0x02` and `audio_format: "opus"`
  are reserved; a session that asks for Opus is downgraded to PCM today.
- **Nightly regression job** running the curated live suite against provider
  keys and diffing benchmark results against a baseline.
- **Prometheus scrape endpoint** next to the JSON metrics API.
- **`ErrorPolicy::Fallback`** so a failing provider can hand over to a
  configured alternative instead of degrading.
- **Active health probing** of external sidecars between sessions.

## Done recently

- Local SLA calibration (`[grading.targets]`), compact Session Summary modal,
  and LLM diagnostic guide with AI agent skills (2026-10).
- Google Cloud STT v2 (Chirp 3) streaming provider (2026-09).
- Speech-to-speech latency measured on first audio byte with anchor provenance
  on the wire (2026-08).
- Streaming-phase throughput metrics and off-path audio annotators (2026-08).
- Client-side tool calling for both cascade and speech-to-speech engines (2026-07).

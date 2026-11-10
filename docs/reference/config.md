# Configuration reference

**Status:** active
**Last verified:** 2026-09-21
**Owner:** Docs

Every section and field of the pipeline TOML file with its type and default.
Source of truth: `voice-pipes/src/config/types.rs` and the `SttConfig`,
`TtsConfig`, `TextProcessorConfig` types in `voice-pipes-sdk`. The docs gate
checks that every section here still exists in the code. For the walkthrough
see the [configuration guide](../guides/configuration.md); for provider
option keys see each [provider page](../guides/providers/README.md).

All sections reject unknown keys except the open option maps noted.

## Loading, merging, validation

- `--config <path>`, or auto-discovery of `voice-pipes.toml` upward from the
  current directory.
- CLI overrides win: `--log-format` → `pipeline.log_format`;
  `--captures-dir` → `pipeline.audit.captures_dir` (and enables the audit
  trail); `--audit-tap` → `pipeline.audit_tap.side`; `--audit-tap-dir` →
  `pipeline.audit_tap.dir`; `--output-rate` → `output.sample_rate`.
- `voice-pipes validate-config` returns every `ConfigError`: its `field`, a `message` for
  people, and a `kind` for code (an invalid value, an unknown name, an unknown key).
- The config hash used for fleet grouping covers everything except
  `[transport]` and audit paths.

## Top level

| Key | Type | Default | Notes |
|---|---|---|---|
| `client_tools_path` | path or list of paths | — | Client-tool catalogs, relative to this file; must precede the first table header; missing or invalid is a validation error |
| `[pipeline]` | table | defaults | Identity and audit |
| `[speculative]` | table | disabled | Speculative execution |
| `[stt]` | table | — | Cascade only |
| `[tts]` | table | — | Cascade only |
| `[[text_processor]]` | array | `[]` | Cascade only; ordered chain |
| `[realtime_provider]` | table | — | Speech-to-speech; mutually exclusive with `[stt]`/`[tts]` |
| `[output]` | table | — | Output rate and chunking |
| `[vad]` | table | — | |
| `[turn_detection]` | table | — | |
| `[barge_in]` | table | enabled | |
| `[adaptive_quality]` | table | disabled | Network-aware fidelity tiers (`serve` only) |
| `[grading]` | table | built-in defaults | Per-pipeline session SLA targets and latency bands |
| `[transport]` | table | disabled | WebSocket server (`serve` only) |
| `[[audio_filter]]` | array | `[]` | Ingress audio filters (pre-VAD, pre-STT noise suppression) |
| `[[audio_post_processor]]` | array | `[]` | Post-TTS chain |
| `[[client_tool]]` | array | `[]` | Inline client tools |
| `[agent]` | table | — | External agent and idle re-engagement |
| `[auth]` | table | env defaults | Runtime auth for served surfaces |

## `[pipeline]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `name` | string | — | Fleet and history identity |
| `log_format` | `"compact"` \| `"json"` | `"compact"` | |
| `min_utterance_chars` | integer | `2` | Shorter committed transcripts are discarded |
| `keep_warm_interval_s` | integer | `300` | An idle worker runs its recogniser once per interval so the model stays resident (recognisers warmed once per process only); `0` disables |
| `text_chunking` | table | defaults | Sentence/aggregation settings for streamed text (SDK `TextChunkingConfig`) |

### `[pipeline.audit]`

| Field | Type | Default |
|---|---|---|
| `enabled` | bool | `false` (the launcher's dev profile sets `true`; other shipped profiles `false`) |
| `captures_dir` | path | — (none) |
| `buffer_size` | integer | `256` |
| `retention_days` | integer | `7` — session capture directories whose newest file is older are deleted at worker start and every 6 h; `0` keeps everything |
| `max_session_bytes` | integer | `536870912` (512 MiB) — per-session capture budget; past it audio artefacts are dropped, text artefacts continue; `0` removes the budget |
| `record_session_audio` | bool | `false` — also write one continuous stereo `session_audio.wav` per session (channel 0 the user before the VAD, channel 1 the agent). The per-utterance files are post-VAD and cut into utterances, so the silence between turns and the relative timing of the two speakers are not recoverable from them; turn-taking models and interruption evaluation need both. Records continuously, including silence |
| `session_audio_max_seconds` | integer | `900` — cap on one session's continuous recording; past it audio is dropped and the recording is marked truncated. About 3.8 MB per minute at 16 kHz stereo |
| `full_prompts` | bool | `false` — record every LLM prompt in full in the trace log instead of its message count and hash |

### `[pipeline.audit_tap]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | |
| `side` | `"before"` \| `"after"` \| `"both"` | — (`both` when enabled) | Which side of resampling to snapshot |
| `dir` | path | `"audit_tap"` | |

## `[speculative]`

| Field | Type | Default |
|---|---|---|
| `enabled` | bool | `false` |
| `speculative_tts` | bool | `false` |
| `min_partial_interval_ms` | integer | `150` |

## `[stt]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `provider` | string | `"parakeet-tdt"` | Factory name or alias |
| `model_dir` | string | — | Local model directory |
| `model` | string | — | Model identifier |
| `model_name` | string | — | Remote model name |
| `api_key` | string | — | Hosted providers; prefer the env var the provider names |
| `endpoint` | string | — | Remote endpoint override |
| `sample_rate` | integer | provider default | Input rate the provider expects |
| `[stt.options]` | table | `{}` | Provider-specific; nested sub-table, open. Local ONNX providers (`parakeet-tdt`, `nemotron-streaming`) take `pool_size` (default 2: session sets loaded, one per concurrent session); hosted streaming providers take `max_reconnect_attempts`, `reconnect_delay_ms`, `reconnect_delay_max_ms` (see the provider pages) |

## `[tts]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `provider` | string | `"piper"` | |
| `model` | string | — | Model path or id |
| `model_name` | string | — | Remote model name |
| `voice` | string | — | Voice or speaker |
| `voices` | string | — | Voice pack path (Kokoro) |
| `api_key` | string | — | |
| `endpoint` | string | — | |
| `sanitize_markdown` | bool | `true` | Dual-track sanitizer: strips Markdown formatting (`**bold**`, `*italic*`, `` `code` ``, `#`, `-`, URLs) from spoken audio while keeping them in display transcripts and LLM history |
| `passthrough_tags` | list of strings | `[]` | Explicit allowlist of inline directives/tags (e.g. `["<break", "<emotion", "[whispers]"]` or `["*"]`) passed to the TTS engine in `spoken_text` and stripped from display transcripts |
| `[tts.lexicon]` | table | `{}` | Whole-word pronunciation substitutions applied to `spoken_text` only (e.g. `"SQL" = "sequel"`); display transcripts retain original spelling |
| `[tts.options]` | table | `{}` | Provider-specific; nested, open |

## `[[text_processor]]`

| Field | Type | Notes |
|---|---|---|
| `type` | string | Factory name: `llm`, `openai-compat`, `grpc`, `agent-grpc`, `echo`, … |
| any other key | — | Flat: captured into the processor's option map (`model`, `system_prompt`, `temperature`, …) |

Network keys of `type = "openai-compat"` (the full list is in the
[provider page](../guides/providers/llm/openai-compatible.md)):

| Key | Type | Default | Notes |
|---|---|---|---|
| `timeout_secs` | int | `30` | Longest wait for the next byte from the server (response headers, then each streamed chunk); not a whole-request budget. Minimum 1. TCP connect has a fixed 5 s budget |
| `max_retries` | int | `2` | Retries on 5xx, 429 and timeouts, 500 ms doubling back-off capped at 8 s per wait. Clamped to 5 |

## `[realtime_provider]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `provider` | string | required | `openai-realtime`, `gemini-live`, `elevenlabs` |
| `model` | string | provider default | |
| `voice` | string | provider default | |
| `vad` | `"server_vad"` \| `"none"` | `"server_vad"` | `none`: the client drives turn boundaries |
| `instructions` | string | — | System / persona prompt at session start |
| `knowledge_base_path` | path | — | UTF-8 file appended to `instructions`; relative to the worker file; unreadable = logged once, ignored |
| `client_overrides` | string[] | `["vad_threshold", "vad_silence_ms", "vad_prefix_padding_ms", "vad_eagerness"]` | `Configure.options` keys a client may override. Others are refused, logged, and listed in `session_ready.negotiated.ignored_options`. `instructions`, when listed, is appended to the operator prompt, never substituted. `client_tools` and `output_chunking_*` are engine keys and always pass. The wizard writes the three VAD knobs and adds `instructions` only when you answer yes to "Allow connected clients to append to the system prompt" |
| `api_key_env` | string | per provider (`OPENAI_API_KEY`, `GEMINI_API_KEY`, `ELEVENLABS_API_KEY`) | |
| `[realtime_provider.options]` | table | `{}` | `output_emit_rate` (every provider: the rate the engine resamples the model's audio to, 8000–48000, default 16000); provider-specific: `thinking_budget` (Gemini 2.x), `thinking_level` (Gemini 3.x), `agent_id` / `agent_id_env` / `public_agent` / `language` / `override_prompt` (ElevenLabs) |

### `[realtime_provider.endpoint_observer]`

vp's own end-of-speech observer on the uplink. Measurement only.

| Field | Type | Default |
|---|---|---|
| `enabled` | bool | `true` |
| `backend` | `"energy"` \| `"silero"` | `"energy"` |
| `model` | path | — (Silero backend) |
| `energy_threshold` | float | `200.0` |
| `speech_prob` | float | `0.5` |
| `hangover_ms` | integer | `300` |
| `min_speech_ms` | integer | `150` |

The observer also meters the user's input on the same pass: speech level, noise floor, SNR and clipping per user turn, as the `input_*` keys on the `realtime` turn metrics. `enabled = false` removes them along with `vp_speech_end_epoch_ms`.

## `[output]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `sample_rate` | integer | — (provider native) | `1..=192000` |
| `[output.chunking] enabled` | bool | `false` | |
| `[output.chunking] frame_ms` | integer | `50` | `10..=250` when enabled |

## `[vad]`

All fields optional; effective defaults are resolved by accessors.

| Field | Type | Effective default | Notes |
|---|---|---|---|
| `mode` | string | `"auto"` | `auto`, `silero`, `energy`, `passthrough`, or a plugin backend's name (registry-driven) |
| `model` | path | — | Required for `silero` |
| `energy_threshold` | float | `200.0` | `0.0..=10000.0` |
| `speech_prob` | float | `0.5` | `0.0..=1.0` |
| `neg_threshold` | float | — | Silero exit threshold (`0.35` in the backend) |
| `min_speech_ms` | integer | — | Consumed by the segmenter |
| `min_silence_ms` | integer | — | Consumed by the segmenter |
| `pre_roll_ms` | integer | `300` | Audio kept from before the detected onset and handed to the recogniser with the utterance. Rounded up to whole 20 ms chunks, one chunk at least |
| `threshold` | float | — | Deprecated fallback for both thresholds |
| `options` | table | — | Settings for a plugin backend (`[vad.options]`); the built-in backends read none. See [Plugin SDK › VAD plugins](../extending/plugin-sdk.md#5a-vad-plugins) |

## `[turn_detection]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `mode` | string | `"silence"` | `silence`, `provider`, `layered`, or a plugin detector: `smart`, `vap` (experimental) |
| `silence_timeout_ms` | integer | `1000` | `> 0`; primary or fallback in every mode |

Every other key belongs to the selected mode's detector, which checks it
and refuses keys it does not read: the built-in modes read none. The two
plugin detectors read:

| Field | Type | Default | Notes |
|---|---|---|---|
| `smart_model_path` | string | — | `smart` only |
| `smart_threshold` | float | `0.5` | `0.0..=1.0` |
| `smart_window_secs` | integer | `8` | 1–8: the model reads the last 8 s |
| `smart_min_audio_ms` | integer | `500` | |
| `vap_model_path` | string | — | `vap` only (experimental); export one with `scripts/export-vap-onnx.py` |
| `vap_threshold` | float | `0.5` | `0.0..=1.0`; confidence that the user has yielded, above which the turn commits |
| `vap_min_interval_ms` | integer | `200` | shortest gap between inferences |

`smart` classifies whether the turn has *already* ended, from the user's
channel, and only once a pause has been paid for. `vap` (**experimental**)
([Voice Activity Projection](https://github.com/ErikEkstedt/VAP), MIT) instead
predicts who will hold the floor over the next two seconds, reading the user
and the agent together, so it can be asked while the user is still speaking
and its answer carries lead time — `confidence` and `expected_end_in` on every
evaluation rather than a verdict after the fact.

The context window is baked into the exported model file, not configured here:
a dynamic-length export does not survive a different input length. Cost is
roughly linear in the window (about 21 ms per run at 2 s, 346 ms at 20 s), so
the checkpoint's native 20 s context is not what you want to run
continuously. Requires the CLI crate's `turn-vap` feature.

The window and threshold defaults are chosen for cost, not measured against
real conversations, and the bundled checkpoint is trained on English. See
[knowing whether it works](../guides/turn-detection-and-interruptions.md#knowing-whether-it-works)
before relying on it.

## `[barge_in]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `true` | |
| `min_chunks` | integer | `3` | `> 0`; ~20 ms per chunk |
| `speech_threshold` | float | `2000.0` | `> 0`; RMS on the i16 scale |

## `[transport]`

| Field | Type | Default |
|---|---|---|
| `enabled` | bool | `false` |
| `bind` | string | `"0.0.0.0"` |
| `port` | integer | `8800` — `> 0` when enabled |
| `max_sessions` | integer | `16` — `>= 1` |
| `write_timeout_ms` | integer | `5000` — `> 0`; a WebSocket frame that cannot be written within this window (the client stopped reading) closes the session instead of stalling it |

Validation also enforces: `pipeline.audit.buffer_size >= 1`;
`agent.idle_timeout_ms > 0` when set; `adaptive_quality.downgrade_observations >= 1`
and the tier thresholds ordered `reduced < minimal < critical` when enabled;
`circuit_breaker.failures >= 1` and `cooldown_ms > 0`;
`endpoint_observer.backend` one of `energy`, `silero` (the latter with `model`),
`energy_threshold` in `[0, 10000]`, `speech_prob` in `[0, 1]`; `vad.min_speech_ms > 0`,
`vad.neg_threshold` in `[0, 1]`; `pool_size >= 1` on any STT or TTS provider;
at most 16 `[[text_processor]]`, 16 `[[audio_post_processor]]` and 64
`[[client_tool]]` entries; `instructions`, `system_prompt` and `idle_message`
at most 32 KB (use `knowledge_base_path` for long material).

Outbound audio on the WebSocket transport is paced, not configurable: the
first 500 ms of each utterance goes out immediately, the rest at 1.5× real
time on a fixed schedule, so control frames such as `clear_audio` are never
queued behind seconds of audio. A `clear_audio` drops whatever is still
queued.

## `[adaptive_quality]`

Off by default. When disabled the controller still publishes what it sees,
but the session's fidelity tier stays `full`.

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | |
| `downgrade_observations` | integer | `2` | Consecutive degraded samples of one signal before the tier drops. Each signal (ping RTT bucket, arrival real-time factor, underruns, socket-write latency) keeps its own count |
| `reduced_arrival_rtf` / `minimal_arrival_rtf` / `critical_arrival_rtf` | float | `1.15` / `1.4` / `2.0` | Client arrival wall time over audio duration |
| `reduced_send_latency_ms` / `minimal_send_latency_ms` / `critical_send_latency_ms` | integer | `250` / `750` / `2000` | Smoothed server socket-write latency, sampled once per second |
| `reduced_underruns` / `minimal_underruns` | integer | `1` / `3` | Playback underruns in one client report |
| `reduced_clean_secs` / `minimal_clean_secs` / `critical_clean_secs` | integer | `30` / `45` / `60` | How long every reported signal must look better than the current tier before climbing one step |

A `poor` ping bucket counts as one tier; `lost` does not vote because the
heartbeat is already closing the session.

## `[[audio_filter]]`

Ingress audio filters that process microphone PCM in place before VAD and STT.
Multiple filters execute in configured array order.

| Field | Type | Default | Notes |
|---|---|---|---|
| `provider` | string | required | Factory name or alias (e.g. `"deepfilter"`, `"deepfilternet3"`) |
| `model` | path | — | ONNX model path when using model-based filter; omitted runs pure-Rust DSP |
| `attenuation_limit_db` | float | `30.0` | Maximum attenuation floor in dB (`0.0..=100.0`) |

## `[[audio_post_processor]]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `type` | string | required | `noop`, `level`, `grpc`, … |
| `mode` | `"inline"` \| `"annotate"` | `"inline"` | |
| `batch_ms` | integer | `160` | annotate |
| `first_batch_ms` | integer | `80` | annotate |
| `max_in_flight` | integer | `2` | annotate |
| `timeout_ms` | integer | `250` annotate / `1000` inline | |
| `warmup` | bool | `true` | annotate |
| `[audio_post_processor.circuit_breaker] failures` | integer | `3` | annotate |
| `[audio_post_processor.circuit_breaker] cooldown_ms` | integer | `5000` | annotate |
| any other key | — | | Flat: passed to the processor |

## `[[client_tool]]`

| Field | Type | Default |
|---|---|---|
| `name` | string | required |
| `description` | string | required |
| `speak_on_result` | bool | `false` |
| `[client_tool.parameters]` | JSON Schema object | — |

## `[agent]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `endpoint` | string | — | `AgentService` gRPC endpoint |
| `idle_timeout_ms` | integer | — (disabled) | Silence before an idle prompt |
| `idle_max_prompts` | integer | `2` | Per silence stretch |
| `idle_message` | string | `"Are you still there?"` | |

## `[grading]`

Optional. The rules this pipeline's sessions are graded by in the admin
console; omitted, the built-in ones apply. Written into every session's
`metadata.json`, so a session is graded by what was in force when it ran.
See the [configuration guide](../guides/configuration.md#grading-optional-advanced).

| Field | Type | Default | Notes |
|---|---|---|---|
| `[grading.targets]` | table of name → number | built-in targets | Any subset of `ttft_ms`, `ttfb_ms`, `endpointing_ms`, `stt_processing_ms`, `draining_handshake_ms`, `rtf_max`, `tokens_per_sec_min`, `barge_in_rtt_ms`, `self_barge_played_ms_max`, `outlier_total_ms_factor`, `tool_roundtrip_ms`, `emit_rtf_max`, `post_budget_ratio_max`, `annotation_lag_budget_ms`, `unanswered_wait_ms`. An unknown name or a non-positive value is a startup error; so is an `endpointing_ms` at or under `[turn_detection] silence_timeout_ms`, which no turn could meet |
| `[grading.levels]` | `good`, `poor`, `unusable` (ms) | `1500`, `3000`, `5000` | Bands on critical-path p90; must be in ascending order |

## `[auth]`

Runtime auth for the surfaces this process serves and the credential it
presents to the orchestrator. Each surface reads its bearer token from an
environment variable; an unset variable leaves that surface open (dev mode)
unless `required` is set. See [Deployment](../guides/deployment.md#authentication).

| Field | Type | Default | Surface |
|---|---|---|---|
| `client_token_env` | string | `"VP_CLIENT_TOKEN"` | End-user WebSocket transport (`/ws`) |
| `admin_token_env` | string | `"VP_ADMIN_TOKEN"` | Orchestrator admin API (`/api/v1/*`) |
| `internal_token_env` | string | `"VP_INTERNAL_TOKEN"` | Worker ↔ orchestrator (`/internal/*`) |
| `required` | bool | `false` (code); `true` in every shipped deploy config and in wizard output | Abort startup when a surface's variable is unset |

## Worker file (`serve --worker-config`)

| Field | Type | Default | Notes |
|---|---|---|---|
| `id` | string | generated | |
| `max_concurrent` | integer | | Capacity reported to the orchestrator |
| `debug_port` | integer | | |
| `debug_bind` | string | | |
| `advertise_address` | string | — | Debug address registered when `debug_bind` is not reachable from the orchestrator |
| `transport_advertise_address` | string | — | Browser-reachable WebSocket address handed out by dispatch |
| `tags` | list of strings | `[]` | Dispatch filters |
| `[orchestrator] endpoint` | string | — (standalone) | |
| `[orchestrator] heartbeat_interval_s` | integer | | |
| `[orchestrator] register_timeout_s` | integer | | |

## Orchestrator file (`orchestrate --orchestrator-config`)

| Field | Type | Default |
|---|---|---|
| `bind` | string | `"127.0.0.1"` |
| `port` | integer | `9090` |
| `cors_origins` | list | `["http://localhost:3000"]` (`["*"]` = any) |
| `benchmarks_dir` | path | `"benchmarks/"` |
| `captures_dir` | path | `"captures/"` |
| `[workers] heartbeat_timeout_s` | integer | `30` | A worker silent this long is marked dead |
| `[workers] reap_interval_s` | integer | `10` | |
| `[workers] dead_grace_s` | integer | `600` | A dead record is removed this long after its last heartbeat plus the timeout; only live records count against `max_registered` |
| `[workers] max_registered` | integer | `1000` | Live worker ids the registry accepts |
| `[workers] allowed_hosts` | list | `[]` | Hosts workers may advertise (empty = any valid `host:port`). A live worker's id cannot be re-registered from a different host regardless |
| `[workers] proxy_timeout_s` | integer | `10` | Bound on a proxied `PATCH /api/v1/workers/{id}/params` |
| `[sessions] max_entries` | integer | |
| `[sessions] store` | table | in-memory | Persistence backend |
| `[dispatch] policy` | string | `"least-connections"` |
| `[dispatch] require_pipeline_match` | bool | `false` |
| `[audit] shared_dir` | path | `"audit/"` |

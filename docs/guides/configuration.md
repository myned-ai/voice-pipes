# Configuration

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

One TOML file describes a pipeline. This page explains how the file is
found, merged with the command line and validated, and walks through every
section with its defaults. Field-by-field tables are in the
[configuration reference](../reference/config.md); provider-specific keys are
on each [provider page](providers/README.md); ready-to-run feature pipeline
examples are indexed in
[`voice-pipes/examples/features/README.md`](../../voice-pipes/examples/features/README.md).

## Where the file comes from

- `--config <path>` on any subcommand, or
- auto-discovery of `voice-pipes.toml` in the current working directory only
  (no parent or home lookup). The worker logs a warning with the absolute
  path when it falls back to this, so a worker started from the wrong place
  cannot silently pick up someone else's file; give `serve` an explicit
  `--config` in deployments.

Every command validates the merged config before doing anything, `serve`
included; the ranges and caps that validation enforces are listed on each
table of the [reference](../reference/config.md). A provider or turn-detection
mode that is not compiled into the binary is an error, never a silent
fallback.

Paths in the file resolve in two ways. `client_tools_path` and
`realtime_provider.knowledge_base_path` are relative to the config file's own
directory, so they can be version-controlled next to it. Every other relative
path (`captures_dir`, model files, service-account keys) is relative to the
working directory the worker was started from; the shipped Docker configs
assume `/app`.

Command-line flags always win over the file. The global flags that override
config are `--log-format`, `--captures-dir` (turns the audit trail on and sets
its directory), `--audit-tap` / `--audit-tap-dir`, and `--output-rate`.

Validate without starting anything:

```sh
voice-pipes --config my-agent.toml validate-config
```

Unknown keys are errors in every section (`deny_unknown_fields`), so a typo
fails fast rather than silently doing nothing. The exceptions are the
provider option maps described below, which are open by design.

## Two engines, one file

A file describes either a **cascade** pipeline (`[stt]`, `[[text_processor]]`,
`[tts]`) or a **speech-to-speech** pipeline (`[realtime_provider]`). The two
are mutually exclusive; validation rejects a file with both. Everything else
(`[pipeline]`, `[transport]`, `[barge_in]`, `[[audio_post_processor]]`,
client tools) applies to both.

## Provider options: nested versus flat

This is the one shape rule worth memorising.

- `[stt]` and `[tts]` have a fixed set of top-level keys (`provider`, `model`,
  `model_dir`, `model_name`, `api_key`, `endpoint`, `voice`, …). Anything
  provider-specific goes in a **nested** sub-table: `[stt.options]`,
  `[tts.options]`.
- `[[text_processor]]`, `[[audio_filter]]`, and `[[audio_post_processor]]` are
  **flat**: after `type = "…"` (or `provider = "…"` on `[[audio_filter]]`),
  every other key is passed to the processor or filter as an option.
- `[realtime_provider]` is nested like STT: fixed keys plus
  `[realtime_provider.options]`.

```toml
[stt]
provider = "google"
model_name = "chirp_3"
[stt.options]
location = "eu"
language_codes = ["hr-HR"]

[[text_processor]]
type = "openai-compat"
base_url = "http://localhost:8081/v1"      # flat: no [text_processor.options]
model = "gemma-4-E2B-it"
```

## Sections

### `[pipeline]`

Identity and audit.

```toml
[pipeline]
name = "my-agent"              # optional; shown in the fleet and session history
log_format = "compact"         # or "json"
min_utterance_chars = 2        # shorter transcripts are discarded as noise
keep_warm_interval_s = 300     # an idle worker decodes silence this often so its model stays resident; 0 disables

[pipeline.audit]
enabled = true
captures_dir = "captures"      # one directory per session; this is also the default
buffer_size = 256
# record_session_audio = true  # also one continuous stereo WAV per session
# session_audio_max_seconds = 900

[pipeline.audit_tap]           # WAV snapshots around resampling, for debugging
enabled = false
# side = "both"                # "before" | "after" | "both"
dir = "audit_tap"
```

### `[stt]`, `[[text_processor]]`, `[tts]` (cascade)

```toml
[stt]
provider = "parakeet-tdt"      # any registered STT factory name or alias
model_dir = "models/parakeet-tdt"

[[text_processor]]             # ordered chain; usually one LLM
type = "openai-compat"
base_url = "http://localhost:8081/v1"
model = "gemma-4-E2B-it"
system_prompt = "You are a helpful voice assistant. Be concise."
max_tokens = 256
temperature = 0.7

[tts]
provider = "kokoro"
model = "models/kokoro-v1.0.onnx"
voices = "models/voices-v1.0.bin"
voice = "af_heart"
```

Hosted providers need an API key; each provider page names the variable. A
literal `api_key` in the file also works but keep keys in the environment.

**`[stt]` is optional for text-input deployments.** Omit it entirely and the
worker serves no speech input — right for sessions that connect with
`mode = "text"` or `mode = "text_in_voice_out"` (type in, hear a spoken reply).
A session whose mode *does* accept audio is then refused at connect with a
`SESSION_CREATE_FAILED` error naming the missing provider, rather than
accepting audio nothing will transcribe.

Note this applies to `serve` only. `live` and `batch` cannot do anything
without speech input, so they still fall back to a default STT provider when
the block is absent.

#### Pronunciation lexicon and Markdown sanitization

Voice Pipes employs a dual-track text sanitizer (`display_text` vs `spoken_text`) to solve two fundamental speech synthesis issues: Markdown syntax leakage and phonetic mispronunciation.

```toml
[tts]
provider = "kokoro"
model = "models/kokoro-v1.0.onnx"
voices = "models/voices-v1.0.bin"
voice = "af_heart"
sanitize_markdown = true            # default: true

# Preserve specific SSML / emotion directives for TTS, stripping them from UI captions
passthrough_tags = ["<break", "[whispers]"]

# Whole-word pronunciation substitutions (applied to spoken audio only)
[tts.lexicon]
"SQL" = "sequel"
"Myned" = "Mined"
"nginx" = "engine x"
```

- **Clean LLM History & Captures:** The conversation history fed back to the LLM and user UI captions retain canonical display text (`"Connect to the **SQL** database"`). Only the synthesized audio hears `"sequel"`, preventing model drift and phonetic spelling pollution.
- **Markdown Stripping:** Headings (`#`), bold/italic (`**text**`), inline code (`` `code` ``), bullet lists, and raw URLs are converted into natural spoken phrasing before hitting TTS, so engines never vocalize raw markdown punctuation.
- **Provider-Aware Inline Directives:** Inline tags supported by your TTS provider (such as Kokoro phonemes `[Kokoro](/kˈoʊ.kə.ɹoʊ/)` or ElevenLabs SSML `<break time="1.5s"/>`) pass through to synthesis while remaining invisible in user transcripts. Unsupported tags are safely stripped.

### `[realtime_provider]` (speech-to-speech)

```toml
[realtime_provider]
provider = "openai-realtime"   # "openai-realtime" | "gemini-live" | "elevenlabs"
model = "gpt-realtime"
voice = "alloy"
vad = "server_vad"             # or "none": the client drives turn boundaries
instructions = "You are a concise voice assistant."
# knowledge_base_path = "kb.md"   # appended to instructions at session start
api_key_env = "OPENAI_API_KEY"

[realtime_provider.options]
output_emit_rate = 16000       # the rate the model's audio enters the pipeline at (default 16000)

[realtime_provider.endpoint_observer]   # vp's own end-of-speech observer, measurement only
enabled = true
backend = "energy"             # or "silero" with `model = "…"`
hangover_ms = 300
```

Requires the `realtime` Cargo feature (on by default).

### `[[audio_filter]]`

An ordered chain of ingress audio filters that run in place on incoming
microphone chunks *before* VAD segmentation and STT. Leave the section out for
zero-overhead passthrough.

```toml
[[audio_filter]]
provider = "deepfilter"        # "deepfilter" (aliases: "deepfilternet", "deepfilternet3")
attenuation_limit_db = 30.0    # maximum noise floor attenuation in dB (0.0..=100.0, default 30.0)
```

Requires the `filter-deepfilter` Cargo feature (on by default in the CLI).
[Guide](vad.md#ingress-noise-suppression-audio_filter).

### `[vad]`

```toml
[vad]
mode = "auto"                  # "auto" | "silero" | "energy" | "passthrough"
model = "models/silero_vad.onnx"
speech_prob = 0.5              # Silero: enter speech above this
neg_threshold = 0.35           # Silero: leave speech below this
energy_threshold = 200.0       # energy mode only
min_speech_ms = 250
min_silence_ms = 100
```

`auto` uses Silero when the model file exists and falls back to energy.
Use `passthrough` with providers that do their own VAD (AssemblyAI, Google).
[Guide](vad.md).

### `[turn_detection]`

```toml
[turn_detection]
mode = "layered"               # "silence" | "provider" | "layered" | "smart" | "vap"
silence_timeout_ms = 700       # primary in silence mode, fallback in the others
# smart mode only:
# smart_model_path = "models/smart-turn-v3.2-cpu.onnx"
# smart_threshold = 0.5
# smart_window_secs = 8
# smart_min_audio_ms = 500
# vap mode only (the context window is baked into the exported model):
# vap_model_path = "models/vap/vap-2s.onnx"
# vap_threshold = 0.5
# vap_min_interval_ms = 200
```

[Guide](turn-detection-and-interruptions.md).

### `[barge_in]`

```toml
[barge_in]
enabled = true
min_chunks = 3                 # consecutive ~20 ms speech chunks before interrupting
speech_threshold = 2000.0      # RMS on the i16 scale
```

### `[output]`

```toml
[output]
sample_rate = 48000            # omit to use the TTS provider's native rate

[output.chunking]              # fixed-size output frames; better barge-in granularity
enabled = true
frame_ms = 50                  # 10..=250
```

The output rate is negotiated per session: an explicit `[output] sample_rate`
wins, then the client's `output_sample_rate` preference, then the provider's
native rate.

### `[transport]`

```toml
[transport]
enabled = true
bind = "0.0.0.0"
port = 8800
max_sessions = 16
```

Only `serve` uses it. `live`, `listen`, `pipe` and the single-component
commands never open a socket.

### `[[audio_post_processor]]`

An ordered chain that runs after TTS. `mode = "inline"` (default) processes
each chunk before it is forwarded, for processors that change samples.
`mode = "annotate"` tees audio to an off-path service and delivers results
as `audio_annotation` frames, for lip-sync and similar metadata.

```toml
[[audio_post_processor]]
type = "level"                 # built-in VU annotator
mode = "annotate"
batch_ms = 160
```

[Audio annotators](audio-annotators.md).

### Client tools

Tools the LLM can call that the *client* fulfils. Declared inline, in a
catalog file, or per session by the client.

```toml
client_tools_path = "commands/client-tools.json"   # top-level key: before the first [table]

[[client_tool]]
name = "navigate"
description = "Navigate the UI to a named screen."
speak_on_result = false
[client_tool.parameters]
type = "object"
```

The `client_tools_path` line must appear before any `[table]` header, or TOML
will parse it as a key of the preceding table. [Tool calling](tool-calling.md).

### `[agent]`

Agentic-system integration: an external gRPC agent and idle re-engagement.

```toml
[agent]
# endpoint = "http://localhost:50060"   # AgentService; tool backend on S2S, brain via type = "agent-grpc" on cascade
idle_timeout_ms = 10000        # absent = no idle prompts
idle_max_prompts = 2
idle_message = "Are you still there?"
```

### `[speculative]`

Overlaps LLM generation with the end of the user's speech by feeding STT
partials early and rolling back on divergence. Off by default. See
[`voice-pipes/examples/features/speculative-execution.toml`](../../voice-pipes/examples/features/speculative-execution.toml).

```toml
[speculative]
enabled = false
speculative_tts = false
min_partial_interval_ms = 150
```

### `[adaptive_quality]`

Network-aware degradation of outbound session traffic (`serve` only; off by
default). When enabled, the controller observes client `arrival_rtf`, playback
`underruns`, ping RTT bucket, and server socket `send_latency_ms`, stepping
down through four fidelity tiers (`full` → `reduced` → `minimal` → `critical`)
after `downgrade_observations` consecutive degraded samples and climbing back
one tier at a time after `*_clean_secs`. When `enabled = false`, diagnostics
are still published while the tier remains pinned at `full`. See
[`voice-pipes/examples/features/adaptive-quality.toml`](../../voice-pipes/examples/features/adaptive-quality.toml).

```toml
[adaptive_quality]
enabled = false
downgrade_observations = 2
reduced_arrival_rtf = 1.15
minimal_arrival_rtf = 1.4
critical_arrival_rtf = 2.0
reduced_send_latency_ms = 250
minimal_send_latency_ms = 750
critical_send_latency_ms = 2000
reduced_underruns = 1
minimal_underruns = 3
reduced_clean_secs = 30
minimal_clean_secs = 45
critical_clean_secs = 60
```

### `[auth]`

Runtime authentication for the WebSocket transport, the debug API and the
internal worker-to-orchestrator credential. When the section is absent the
defaults come from `VP_CLIENT_TOKEN` and `VP_INTERNAL_TOKEN`. See
[Deployment](deployment.md#authentication).

### `[grading]` (optional, advanced)

How this pipeline's sessions are graded in the console. Leave it out and
sessions are graded by the built-in targets, which describe a good
conversational agent in general. Set it when your deployment has chosen
otherwise **on purpose** — a longer `silence_timeout_ms` so callers are never
cut off mid-thought, a larger model that answers more slowly. Graded against
the defaults, that pipeline would report `fair` on every session, and a verdict
that is always amber stops being read.

```toml
[grading.targets]              # any subset; what you omit keeps its default
endpointing_ms = 800           # default 500. Must sit above silence_timeout_ms
ttft_ms = 500                  # default 300 (cloud LLMs already default higher)

[grading.levels]               # bands on the response time a caller feels
good = 2000                    # default 1500: at or under this can be "healthy"
poor = 4000                    # default 3000: over this is "degraded"
unusable = 6000                # default 5000: over this is "broken"
```

Speech-to-speech pipelines already default to `ttfb_ms = 1200` and
`endpointing_ms = 1200`, what a healthy cloud realtime model (Gemini Live,
OpenAI Realtime, ElevenLabs Agents) meets; the cascade defaults of 400 and
500 would flag every turn. They are defaults, not custom rules, so a verdict
graded by them carries no custom-rules note; a `[grading.targets]` of your
own, or the console's `ADMIN_GRADING_TARGETS`, still wins.

`[grading.levels]` applies to critical-path p90 — the wait from the end of the
caller's speech to the first audio of the reply. The target names are
`ttft_ms`, `ttfb_ms`, `endpointing_ms`, `stt_processing_ms`,
`draining_handshake_ms`, `rtf_max`, `tokens_per_sec_min`, `barge_in_rtt_ms`,
`self_barge_played_ms_max`, `outlier_total_ms_factor`, `tool_roundtrip_ms`,
`emit_rtf_max`, `post_budget_ratio_max`, `annotation_lag_budget_ms` and
`unanswered_wait_ms`. A name
that is not one of these, a value that is not a positive number, or bands out
of order (`good` < `poor` < `unusable`) is a startup error: a misspelt target
would otherwise never apply, with nothing to tell you so.

Three things worth knowing:

- **It travels with the session.** The rules are written into each session's
  capture, so a session is always graded by what was in force when it ran.
  Editing this section never re-grades history.
- **A custom grade always says so.** The verdict names every value that differs
  from its default — *"Graded against custom rules: endpointing_ms 800 (default
  500, from pipeline config)"* — and the session page marks the badge
  `custom rules`. Relaxing a target changes the grade; it cannot change it
  quietly.
- **It does not hide real failures.** Severity is still measured against your
  target, so a 3 s wait against an 800 ms target is as `degraded` as it would
  be against 500.

A console-wide default for pipelines that set none of their own can be given to
the admin backend as `ADMIN_GRADING_TARGETS` and `ADMIN_GRADING_LEVELS`
(`name:value` pairs, comma-separated). A pipeline's own `[grading]` wins over
it, and a `?targets=` on a report request wins over both.

## Worker and orchestrator files

`serve` and `orchestrate` take a second file for process-level settings
(ports, capacity, the orchestrator endpoint). They are separate on purpose:
the pipeline file describes *what runs*, the worker file describes *where*.
See [Fleet](fleet.md).

## Configuration hash

Workers report a 32-character hash of the pipeline-relevant settings
(everything except `[transport]` and audit paths). The orchestrator groups
workers by it, can require a hash match when dispatching, and the admin
console diffs two hashes. Two workers with the same file are interchangeable
by definition.

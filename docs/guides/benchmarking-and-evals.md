# Benchmarking Guide

**Status:** active
**Last verified:** 2026-09-06 (moved into the public tree; content unchanged)
**Owner:** Docs

Compare voice-pipeline configurations side-by-side with deterministic audio
replay, automated scoring, and built-in visualization.

---

## Quick Start

Run a two-variant benchmark in under 5 minutes.

### Prerequisites

| Requirement | Check |
|---|---|
| Orchestrator built | `cargo build --release` |
| At least one WAV file | `hello.wav` in the working directory |
| Local LLM server running | `curl http://localhost:8081/health` returns 200 |
| Orchestrator running | `cargo run --release -- orchestrate --config orchestrator.toml` |

### 1. Run the multi-variant script

```powershell
.\run-benchmark-multi.ps1
```

This starts two workers (each with a different pipeline config), creates a test
suite from `hello.wav`, creates an experiment with both variants, runs it, and
prints a comparison table when done.

### 2. View results in the Admin UI

Open http://localhost:5173/benchmarks and click on the experiment.

That's it. Read on for customization, advanced modes, and the full API.

---

## Table of Contents

- [Concepts](#concepts)
- [Pipeline Configuration](#pipeline-configuration)
- [Worker Configuration](#worker-configuration)
- [PowerShell Scripts](#powershell-scripts)
  - [Single Variant](#single-variant-benchmark)
  - [Multi-Variant (Parallel)](#multi-variant-parallel-benchmark)
  - [Multi-Variant (Sequential)](#multi-variant-sequential-benchmark)
  - [OpenAI vs Local LLM](#openai-vs-local-llm)
- [REST API Reference](#rest-api-reference)
  - [Test Suites](#test-suites)
  - [Experiments](#experiments)
  - [Running & Results](#running--results)
  - [Partial Reruns](#partial-reruns)
- [Scoring & Metrics](#scoring--metrics)
- [Admin UI](#admin-ui)
- [Storage Layout](#storage-layout)
- [Troubleshooting](#troubleshooting)

---

## Concepts

| Term | Description |
|------|-------------|
| **Experiment** | An A/B (or A/B/C/...) comparison. References a test suite and one or more variants. Status: `draft` -> `running` -> `complete` \| `failed`. |
| **Variant** | A frozen pipeline configuration (TOML snapshot + SHA-256 hash). Dispatch tags route the variant to the correct worker at runtime. |
| **Test Suite** | A reusable collection of scenarios. Suites can be shared across experiments. |
| **Scenario** | A single test input - either an `audio_file` (WAV replayed through the pipeline) or a `text_prompt` (bypasses STT). Optionally includes expected outcomes for automated scoring. |
| **Repetition** | Each scenario runs N times per variant. Metrics are aggregated across repetitions. |
| **Tag dispatch** | Workers register with tags (e.g. `variant:openai`). The runner finds the least-loaded worker whose tags are a superset of the variant's required tags. |

### Status Lifecycle

```
draft ──POST /run──> running ──success──> complete
                         │
                         └──error──> failed
```

Completed or failed experiments can be **partially re-run** using the
`?variants=` parameter (see [Partial Reruns](#partial-reruns)).

---

## Pipeline Configuration

Each variant needs a TOML pipeline config. This controls which STT, LLM, and
TTS providers are used, along with VAD settings, turn detection, and barge-in.

Example (`examples/test-live-gemma4.toml`):

```toml
[pipeline]
name = "live-gemma4"

[transport]
enabled = true
port = 8800                       # Each variant needs a unique port

[vad]
mode = "silero"
model = "models/silero_vad.onnx"

[stt]
provider = "parakeet-tdt"
model_dir = "models/parakeet-tdt"

[turn_detection]
mode = "layered"
silence_timeout_ms = 1000

[barge_in]
enabled = true

[tts]
provider = "piper"
model = "models/en_US-lessac-medium.onnx"

[[text_processor]]
type = "openai-compat"
model = "gemma-4-E2B"
base_url = "http://localhost:8081"
system_prompt = "You are a friendly voice assistant."
max_tokens = 128
```

When comparing cloud vs local, create a second config pointing at a different
provider:

```toml
# examples/bench-openai.toml  (only the differences)
[transport]
port = 8801                       # Different port

[[text_processor]]
type = "openai-compat"
model = "gpt-4o-mini"             # Uses OPENAI_API_KEY env var
max_tokens = 128
```

---

## Worker Configuration

Each worker needs a TOML file with dispatch tags so the benchmark runner can
route variants to the right process.

```toml
# worker-bench-a.toml
debug_port = 8085
tags = ["variant:default"]

[orchestrator]
endpoint = "http://127.0.0.1:9090"
```

```toml
# worker-bench-openai.toml
debug_port = 8086
tags = ["variant:openai"]

[orchestrator]
endpoint = "http://127.0.0.1:9090"
```

Rules:
- Each worker must have a **unique `debug_port`**
- Tags must be **unique per variant** — the runner uses them for dispatch
- The pipeline config's `transport.port` must also be unique per worker

> **Port fallback:** If a configured port is already in use, the worker
> automatically tries the next port (up to 16 attempts) and logs a warning.
> The actual bound port is sent to the orchestrator at registration time, so
> routing works even when ports shift. If all 16 ports are busy, startup fails
> with a clear error message.

---

## PowerShell Scripts

All scripts live in the `voice-pipes/` directory. They automate the full
workflow: health checks, worker management, suite/experiment creation, execution,
and result display.

### Common Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `-OrchestratorUrl` | `http://127.0.0.1:9090` | Orchestrator API base URL |
| `-AudioFiles` | `@("hello.wav")` | WAV files to use as scenarios |
| `-Repetitions` | `3` | Runs per scenario per variant |
| `-TimeoutS` | `120` | Per-scenario timeout (seconds) |
| `-PollIntervalS` | `5` | How often to check status |
| `-MaxWaitS` | `600` | Max wait before giving up |
| `-LlmServerUrl` | `http://localhost:8081` | Local LLM health-check URL |

### Authentication

The orchestrator started from the shipped configs requires `VP_ADMIN_TOKEN`
on its API. Every benchmark script dot-sources `scripts/admin-auth.ps1`,
which takes the token from the environment or from the launcher-generated
`voice-pipes/.env` and sets it as the default `Authorization` header, so no
flag is needed after one launcher run. `scripts/s2s_probe.py` resolves
`VP_CLIENT_TOKEN` the same way (or pass `--token`).

### Single-Variant Benchmark

```powershell
# Benchmark a single pipeline config
.\run-benchmark.ps1

# With custom audio
.\run-benchmark.ps1 -AudioFiles hello.wav,test_speech.wav -Repetitions 5
```

Runs one variant only. Useful for baseline measurements or regression testing.

### Multi-Variant Parallel Benchmark

```powershell
# Default: Gemma 4 default vs Gemma 4 concise
.\run-benchmark-multi.ps1

# Custom variants
.\run-benchmark-multi.ps1 `
    -VariantA_Config examples/test-live-gemma4.toml `
    -VariantA_Name "Gemma 4 E2B" `
    -VariantB_Config examples/bench-openai.toml `
    -VariantB_Name "OpenAI GPT-4o-mini" `
    -VariantB_WorkerConfig worker-bench-openai.toml `
    -AudioFiles hello.wav,test_speech.wav `
    -Repetitions 5

# If workers are already running
.\run-benchmark-multi.ps1 -SkipWorkers
```

Starts **both workers simultaneously**, then runs all variants. Fast, but shared
resources (GPU, CPU) can skew STT/TTS metrics when one variant uses a local LLM
and the other does not.

**Parameters specific to multi:**

| Parameter | Default | Description |
|-----------|---------|-------------|
| `-VariantA_Config` | `examples/test-live-gemma4.toml` | Pipeline TOML for variant A |
| `-VariantA_Name` | `"Gemma 4 Default"` | Display name |
| `-VariantA_WorkerConfig` | `worker-bench-a.toml` | Worker TOML for variant A |
| `-VariantB_Config` | `examples/bench-gemma4-concise.toml` | Pipeline TOML for variant B |
| `-VariantB_Name` | `"Gemma 4 Concise"` | Display name |
| `-VariantB_WorkerConfig` | `worker-bench-b.toml` | Worker TOML for variant B |
| `-SkipWorkers` | (switch) | Skip starting/stopping workers |

### Multi-Variant Sequential Benchmark

```powershell
# Default: Gemma 4 vs OpenAI, one at a time
.\run-benchmark-sequential.ps1

# Custom audio and repetitions
.\run-benchmark-sequential.ps1 -AudioFiles hello.wav,test_speech.wav -Repetitions 5
```

Runs variants **one at a time**: starts Worker A, runs variant A, stops
Worker A, then starts Worker B, runs variant B, stops Worker B. Results are
merged into a single experiment.

This eliminates resource contention - STT and TTS metrics are not skewed by the
other variant's LLM server consuming GPU/CPU. Use this mode when comparing a
local LLM against a cloud LLM and you want clean STT/TTS numbers.

Uses the `?variants=` query parameter under the hood to run one variant per
pass and merge results automatically.

### OpenAI vs Local LLM

```powershell
# Preconfigured OpenAI vs Gemma 4 comparison
.\run-benchmark-openai-vs-gemma.ps1

# More audio files
.\run-benchmark-openai-vs-gemma.ps1 -AudioFiles hello.wav,test_speech.wav -Repetitions 5
```

A convenience script with OpenAI and Gemma 4 defaults already wired up.
Requires `OPENAI_API_KEY` environment variable. Uses higher timeouts (180s per
scenario, 900s max wait) since cloud calls can be slower.

### S2S latency probe (`scripts/s2s_probe.py`)

A scripted caller for realtime (speech-to-speech) latency work that needs no
orchestrator, suite or experiment: it replays a spoken segment of any WAV —
a `captures/<session>/NNN_mic_raw.wav` from a real session is ideal — into a
worker exactly the way the Playground does (48 kHz i16 binary frames,
realtime-paced, browser-sized 2.667 ms chunks by default) and stamps every hop
against the **true** end of speech, i.e. the last speech sample it put on the
wire. That anchor is the one thing no provider-reported timestamp can move,
which is what makes the columns comparable across providers and VAD settings.

```bash
pip install websockets
python scripts/s2s_probe.py --wav captures/<id>/002_mic_raw.wav --start-s 18.2 --end-s 19.5 --repeat 3
python scripts/s2s_probe.py ... --chunk-ms 20            # uplink batching A/B
python scripts/s2s_probe.py ... --verbose                # full hop log per run
python scripts/s2s_probe.py ... --json runs.jsonl        # one row per run
```

Columns (ms after the true end of speech): `anchor` = the end-of-speech stamp
the server anchored TTFB on (`user_speech_end_epoch_ms`; its provenance is the
`source` column — `input_transcript_proxy` for Gemini, `provider_speech_stopped`
for OpenAI, `vp_vad` when only vp's own observer had one), `speak` = the
server's `state_changed: speaking` (first response content, text or audio),
`audio` = first audio frame with samples at the client, `ttfb` = the server's
`ttfb_ms` (anchor → first audio), `text` = `first_text_ms`, `endpt` =
`provider_endpointing_ms` (anchor − vp's own end-of-speech stamp; the S2S
endpointing cost). See
[VPO telemetry §5.6](../reference/vpo-telemetry.md) for the
definitions; `anchor + ttfb ≈ audio` is the invariant a healthy worker keeps
(`speak` may lead `audio` by Gemini's transcript-before-audio gap — that is the
model, not vp).

Raw provider frames (ordering of `inputTranscription` / `outputTranscription` /
`inlineData` / `turnComplete`) are logged at trace level by the realtime WS
reader, text and binary-JSON frames alike:
`RUST_LOG=voice_pipes::realtime=trace,voice_pipes=info` on the worker
(`worker-custom` / `worker-custom-gpu` pass `RUST_LOG` through).

---

## REST API Reference

All endpoints live under the orchestrator (default `http://localhost:9090`).
Responses are wrapped in `{ "data": ... }`.

### Test Suites

**Create a suite:**

```bash
curl -X POST http://localhost:9090/api/v1/benchmark/suites \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Customer Support Basics",
    "description": "Common greetings and requests",
    "scenarios": [
      {
        "id": "greeting",
        "name": "Simple greeting",
        "input": { "type": "audio_file", "path": "/absolute/path/to/greeting.wav" },
        "expected": {
          "transcript_contains": ["hello", "hi"],
          "max_latency_ms": 2000
        }
      },
      {
        "id": "refund",
        "name": "Refund request",
        "input": { "type": "audio_file", "path": "/absolute/path/to/refund.wav" },
        "expected": {
          "transcript_contains": ["refund"],
          "response_intent": "Acknowledge refund and ask for order number",
          "golden_transcript": "I would like a refund for my last order please"
        }
      },
      {
        "id": "hours",
        "name": "Business hours (text only)",
        "input": { "type": "text_prompt", "text": "What are your business hours?" }
      }
    ]
  }'
```

**List / Get / Update / Delete:**

```bash
curl http://localhost:9090/api/v1/benchmark/suites
curl http://localhost:9090/api/v1/benchmark/suites/{suite_id}
curl -X PUT http://localhost:9090/api/v1/benchmark/suites/{suite_id} \
  -H "Content-Type: application/json" -d @suite.json
curl -X DELETE http://localhost:9090/api/v1/benchmark/suites/{suite_id}
```

#### Scenario Input Types

| Type | Fields | Description |
|------|--------|-------------|
| `audio_file` | `path` (string) | Path to a WAV file (16 kHz mono recommended). Replayed through the full pipeline. |
| `text_prompt` | `text` (string) | Text injected directly, bypassing VAD and STT. Useful for testing LLM + TTS in isolation. |
| `tts_only_corpus` | `entries[]` of `{ id, text, language?, bucket, class }` | Each entry is synthesized directly by the variant's TTS provider (no STT, no LLM) over one shared session. Per-entry TTFB / synthesis time / RTF, the produced WAV (playable in the UI), and **speech stats**: `peak_amplitude` and `speech_ratio` (share of 100 ms windows with speech energy). An entry with audio but no speech is flagged `silent` and fails the scenario — this is how a sampling TTS that runs to its length cap in silence (seen with a fine-tuned Pocket-TTS checkpoint) shows up as a number instead of a listener's impression. `bucket` ∈ `short`/`medium`/`long`, `class` ∈ `plain`/`numerics`/`punctuation`/`mixed`. Repeat a sentence as several entries to measure its failure rate. |
| `conversation` | `turns[]` of `{ id, input, expected?, expect_followups?, followup_timeout_ms? }` | Multi-turn scripted conversation over a single WebSocket session so server-side history is preserved. Each turn's `input` is `{ "type": "audio", "path": "..." }` or `{ "type": "text", "text": "..." }`. See [`conversation-basic.json`](../../voice-pipes-bench/examples/benchmark/conversation-basic.json). |
| `barge_in` | `initial`, `interrupt: { mode, fire_on }`, `follow_up?` | Speaks an `initial` turn, fires an interrupt when `fire_on` triggers (`tts_started`, `first_audio`, or `{ "kind": "playback_offset", "offset_ms": N }`), and optionally sends a `follow_up` turn to verify recovery. `interrupt.mode` supports `client_cancel`, `{ "kind": "inject_audio", "path": "..." }`, and `{ "kind": "inject_text", "text": "..." }`. See [`barge-in-cancel.json`](../../voice-pipes-bench/examples/benchmark/barge-in-cancel.json) and [`barge-in-inject-audio.json`](../../voice-pipes-bench/examples/benchmark/barge-in-inject-audio.json). |
| `synthetic_caller` | `config: { caller, persona, persona_template?, stop, barge_in?, bridge? }` | Drives a multi-turn free-form conversation using an AI caller (`mock`, `openai_realtime`, or `gemini_live`) until `stop` conditions (`max_turns`, `max_duration_s`, `max_caller_silence_s`) fire. See [Synthetic Caller](#synthetic-caller-s2s-multi-turn). |

#### Expected Outcomes (all optional)

| Field | Applies To | Type | Scoring |
|-------|------------|------|---------|
| `transcript_contains` | Single-turn / Turn | `string[]` | Pass if all substrings appear in STT/response output (case-insensitive) |
| `max_latency_ms` | Single-turn / Turn | `u64` | Pass if E2E latency is below this threshold |
| `golden_transcript` | Single-turn / Turn | `string` | Computes WER (word error rate) and CER (character error rate) via Levenshtein distance |
| `golden_response` | Single-turn | `string` | For exact/fuzzy response matching |
| `response_intent` | Single-turn | `string` | Expected intent description (for LLM-as-judge scoring) |
| `turns_completed` | `conversation` | `u32` | Minimum number of assistant turns that must reach `TurnCompleted` |
| `max_latency_ms_per_turn` | `conversation` | `u64` | Per-turn E2E latency cap across all turns in the conversation |
| `max_latency_drift_pct` | `conversation` | `f64` | Max tolerated latency increase across turns as a `%` of turn 1 (e.g. `50.0`) |
| `must_interrupt` | `barge_in` | `bool` | Pass only if an `Interrupted` event is observed |
| `interruption_trigger` | `barge_in` | `string` | Expected trigger: `server_vad`, `client_cancel`, `early_barge`, or `any` |
| `max_barge_in_rtt_ms` | `barge_in` | `u64` | Max acceptable ms from firing the interrupt to receiving `Interrupted` |
| `max_recovery_ms` | `barge_in` | `u64` | Max acceptable ms from `Interrupted` until the pipeline returns to `Listening` |
| `follow_up_must_complete` | `barge_in` | `bool` | When `follow_up` (or voice interrupt reply) is present, it must reach `TurnCompleted` |
| `min_completed_turns` | `synthetic_caller` | `u32` | Minimum completed pipeline turns required before stop condition fires |
| `pipeline_must_say` | `synthetic_caller` | `string[]` | Substrings that must appear in at least one pipeline turn (case-insensitive) |
| `max_pipeline_turn_latency_ms` | `synthetic_caller` | `u64` | Per-pipeline-turn E2E latency cap |
| `max_barge_in_failure_rate` | `synthetic_caller` | `f64` | Maximum fraction (`0.0..=1.0`) of failed forced barge-in attempts |

### Experiments

**List available pipeline configs across workers:**

```bash
# All configs reported by registered workers (falls back to local examples/ in dev)
curl http://localhost:9090/api/v1/benchmark/configs

# Filter to workers matching specific tags
curl "http://localhost:9090/api/v1/benchmark/configs?tags=variant:openai"
```

**Create an experiment:**

```bash
curl -X POST http://localhost:9090/api/v1/benchmark/experiments \
  -H "Content-Type: application/json" \
  -d '{
    "name": "LLM Provider Comparison",
    "test_suite_id": "<suite_id>",
    "variants": [
      {
        "id": "gemma-local",
        "name": "Gemma 4 E2B (local)",
        "config_path": "examples/test-live-gemma4.toml",
        "config_hash": "",
        "tags": ["variant:default"]
      },
      {
        "id": "openai-cloud",
        "name": "OpenAI GPT-4o-mini",
        "config_path": "examples/bench-openai.toml",
        "config_hash": "",
        "tags": ["variant:openai"]
      }
    ],
    "repetitions": 3
  }'
```

> **Note:** `config_hash` can be empty - the runner computes and stores the
> SHA-256 hash automatically when the experiment starts.

**List / Get / Delete:**

```bash
curl http://localhost:9090/api/v1/benchmark/experiments
curl http://localhost:9090/api/v1/benchmark/experiments/{id}
curl -X DELETE http://localhost:9090/api/v1/benchmark/experiments/{id}
```

### Running & Results

**Run an experiment:**

```bash
curl -X POST "http://localhost:9090/api/v1/benchmark/experiments/{id}/run"
```

Returns `202 Accepted` immediately. The runner executes in the background.

**Query parameters:**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `speed` | float | `0.0` | Audio injection speed. `0.0` = as fast as possible, `1.0` = real-time, `2.0` = 2x speed |
| `timeout_s` | u64 | `120` | Per-scenario timeout in seconds |
| `variants` | string | (all) | Comma-separated variant IDs to run. Omit to run all variants. |
| `scenarios` | string | (all) | Comma-separated scenario IDs to run. Preserves existing results for skipped scenarios on reruns. |
| `worker_id` | string | (none) | Pin this run to a specific `worker_id` (bypasses tag-based selection; requires the worker to be Active with spare capacity). |
| `force` | bool | `false` | When `true`, allows re-running a `complete` or `failed` experiment across all variants without specifying `?variants=`, overwriting `results.json`. |

**Poll for results:**

```bash
# Check experiment status
curl http://localhost:9090/api/v1/benchmark/experiments/{id}

# Get full results (available once complete)
curl http://localhost:9090/api/v1/benchmark/experiments/{id}/results
```

**Get a variant's frozen config snapshot:**

```bash
curl http://localhost:9090/api/v1/benchmark/experiments/{id}/variants/{variant_id}/config
# Returns text/plain TOML
```

**Stream synthesized WAV for a TTS-only corpus entry:**

```bash
curl http://localhost:9090/api/v1/benchmark/experiments/{id}/variants/{variant_id}/scenarios/{scenario_id}/entries/{entry_id}/audio \
  --output entry.wav
```

### Partial Reruns

Re-run specific variants or scenarios without re-running the entire experiment.
This is how the sequential benchmark script works under the hood.

```bash
# Run only variant A
curl -X POST "http://localhost:9090/api/v1/benchmark/experiments/{id}/run?variants=gemma-local"

# Later, run only variant B (results from variant A are preserved)
curl -X POST "http://localhost:9090/api/v1/benchmark/experiments/{id}/run?variants=openai-cloud"

# Re-run a single scenario on a specific variant
curl -X POST "http://localhost:9090/api/v1/benchmark/experiments/{id}/run?variants=gemma-local&scenarios=refund"

# Force a full rerun of a completed/failed experiment
curl -X POST "http://localhost:9090/api/v1/benchmark/experiments/{id}/run?force=true"
```

Partial reruns (`?variants=` or `?force=true`) are allowed on experiments with
status `complete` or `failed`. When filtering by `variants` or `scenarios`, the
runner preserves existing results for non-selected variants/scenarios and
replaces only the newly executed ones.

Use cases:
- **Sequential execution** - run one variant at a time to avoid resource
  contention between workers
- **Re-run a failed variant or scenario** - fix the issue and re-run just that slice
- **Add results incrementally** - run variant A today, variant B tomorrow

---

## Comparing turn detectors on your own calls

Every voice framework ships a turn detector. Almost none ship a way to find
out whether it works on *your* calls, so the usual approach is to adopt a model
on faith and tune it by ear.

Record some sessions with
[`record_session_audio`](observability.md#continuous-session-audio), then:

```sh
VP_TURN_SESSIONS=voice-pipes/captures   cargo test --release -p voice-pipes-bench --test turn_detection -- --ignored --nocapture
```

```
detector            turns  median ms     p90 ms   cut-off   forced    hang
--------------------------------------------------------------------------
silence@500ms           5        500        500      0.0%   100.0%    0.0%
silence@700ms           5        700        700      0.0%   100.0%    0.0%
```

Add `--features turn-vap` (it links the `voice-pipes-turn-vap` plugin crate)
and `VP_VAP_MODEL=<export>.onnx` to include the anticipatory detector and
sweep its threshold.

**Where the ground truth comes from.** No hand labelling: an offline
voice-activity pass over the user's channel. A live detector cannot see the
future — that is the whole difficulty of turn detection — while an offline one
can set its threshold from the recording's own noise floor and look forward to
decide whether a gap is a pause or an ending. That asymmetry is exactly what
makes it a fair reference.

**Reading the columns.**

- **median / p90 ms** — how long past the real end of the turn the detector
  waited. This is dead air the caller hears. Commits made *before* the user
  finished are excluded, because averaging those in makes a detector that cuts
  people off look like the quickest one.
- **cut-off** — the share of turns committed while the user was still talking.
  This is the failure latency is traded against; read it together with the
  latency, never alone.
- **forced** — reached the silence fallback instead of deciding. A model-based
  mode with a high forced rate is behaving as `silence`, whatever it is
  configured as.
- **hang** — no commit at all. Usually means the detector never saw what it
  needed rather than that it was slow.

`provider` mode is not comparable this way: its whole input is the
`is_end_of_turn` flag a recogniser sets, and replay has no recogniser.

One caveat worth taking seriously: a model-based detector scored against audio
it has never heard the like of will show a high hang rate, and that is a
statement about the audio, not the detector. If every model-based row hangs,
check the recordings before concluding anything.

## Scoring & Metrics

### Per-Scenario Metrics

| Metric | Source | Description |
|--------|--------|-------------|
| `e2e_latency_ms` | `TurnMetrics` | Total time from audio injection start to last TTS byte |
| `ttft_ms` | `TurnMetrics` | Time to first LLM token (STT + LLM first-token latency) |
| `ttfb_ms` | `TurnMetrics` | Time to first TTS byte (STT + LLM + TTS first-byte) |
| `stt_latency_ms` | `TurnMetrics` | STT inference time |
| `llm_generation_ms` | `TurnMetrics` | LLM generation time |
| `tts_ttfb_ms` | `TurnMetrics` | TTS time-to-first-byte |
| `tokens_per_second` | `TurnMetrics` | LLM token generation throughput |
| `stt_text` | STT output | Raw transcript |
| `response_text` | LLM output | Full LLM response text |
| `scoring.wer` | Computed | Word error rate vs golden transcript (0.0 = perfect) |
| `scoring.cer` | Computed | Character error rate vs golden transcript (0.0 = perfect) |
| `scoring.expectations_met` | Computed | Whether all expected checks passed |
| `scoring.expectation_failures` | Computed | Human-readable failure descriptions (transcript_contains, latency gate, etc.) |

### Recognition accuracy on a corpus

The per-turn diagnostic grades latency, so a provider that answers quickly
with the wrong words still reads as healthy. To measure the words, score a
corpus of WAV files against reference transcripts with the same WER/CER
metrics the benchmark uses:

```sh
VP_STT_CORPUS=models/stt-corpus-hr/corpus.json \
VP_TEST_PARAKEET_DIR=models/parakeet-tdt-v3 \
  cargo test --release -p voice-pipes-bench --test stt_corpus -- --ignored --nocapture
```

The corpus is a JSON array of `{id, source, wav, reference}` with paths
relative to `voice-pipes/`. `source` is a free-form group label; the test
prints a per-entry table and mean error rates per group, which is what
separates "the model is wrong for this language" from "this recording is
too hard for this model". Each source must keep its mean word error rate
under its own budget: `synthetic` 0.15 and `microphone` 0.85 by default,
any other source `VP_STT_MAX_WER` (default `0.35`), and
`VP_STT_MAX_WER_<SOURCE>` (the source upper-cased) overrides one. On the
Croatian corpus Parakeet v3 measures 0.08 on synthetic speech and 0.78 on
the short microphone turns, where it detects the wrong language (English,
Portuguese) and cannot be told the right one.

Build the reference side from text you synthesised (the text *is* the
reference) or from a stronger recogniser's output on real recordings; a
reference produced by one provider cannot be used to score that same
provider.

### Per-Variant Summary

| Metric | Description |
|--------|-------------|
| `avg_e2e_latency_ms` | Mean E2E latency across all scenarios and repetitions |
| `p50_e2e_latency_ms` | Median E2E latency |
| `p90_e2e_latency_ms` | 90th percentile E2E latency |
| `p99_e2e_latency_ms` | 99th percentile E2E latency |
| `avg_ttft_ms` | Mean time to first LLM token |
| `avg_ttfb_ms` | Mean time to first TTS byte |
| `avg_stt_latency_ms` | Mean STT inference time |
| `avg_llm_generation_ms` | Mean LLM generation time |
| `avg_tts_ttfb_ms` / `p50` / `p90` / `p99_tts_ttfb_ms` | Mean and percentiles for TTS time-to-first-byte |
| `avg_tts_synthesis_ms` / `cold` / `warm` | Mean TTS synthesis time (overall, cold-start reps, and warm reps) |
| `avg_tts_rtf` / `avg_tts_chars_per_second` | Mean TTS real-time factor and character throughput |
| `tts_failure_rate` | Fraction of repetitions where TTS failed to produce audio |
| `avg_tokens_per_second` | Mean LLM throughput |
| `avg_wer` | Mean word error rate (if golden transcripts or TTS round-trip scorer provided) |
| `conversation_scenarios` / `avg_turns_completed` / `avg_turn_latency_ms` | Multi-turn conversation aggregates |
| `barge_in_scenarios` / `barge_in_success_rate` | Barge-in count and fraction of scenarios that interrupted as expected |
| `p50` / `p95_barge_in_rtt_ms` | Percentiles of interrupt round-trip latency |
| `p50` / `p95_recovery_ms` | Percentiles of post-interrupt recovery time to `Listening` |
| `scenarios_run` / `passed` / `failed` | Pass/fail counts |

### Pipeline Config Summary

Each variant result includes a `config_summary` extracted from the frozen TOML:

- STT provider and model
- LLM model, base URL, temperature, max tokens, system prompt
- TTS provider and model
- VAD mode, turn detection mode, silence timeout, barge-in setting

This makes it easy to see exactly what differed between variants.

---

## Admin UI

The admin console (http://localhost:5173) has three benchmark pages.

### Experiments List (`/benchmarks`)

- Lists all experiments with status badges (Draft, Running, Complete, Failed)
- Shows variant count, scenario count, creation date
- Checkbox selection for cross-experiment comparison
- Click "Compare Selected" to open the comparison view

### Experiment Detail (`/benchmarks/:id`)

- **Variant cards** - side-by-side summary for each variant showing:
  - P50/P90/P99 E2E latency
  - Average E2E latency, TTFT, TTFB
  - Pipeline config summary (STT, LLM, TTS providers)
- **Latency charts** - percentile distribution, TTFT/TTFB bar charts,
  per-scenario scatter plots
- **Scenario table** - per-scenario results with inline metrics, STT text,
  LLM response, pass/fail status
- **Config drawer** - click a variant to view its full frozen TOML config
  (syntax-highlighted)
- **Config comparison table** - structural diff of variant configs

### Cross-Experiment Comparison (`/benchmarks/compare?ids=a,b,c`)

- Select 2+ experiments from the list to compare across runs
- Trend lines for variant latencies over time
- Useful for tracking regressions or improvements across config changes

---

## Storage Layout

```
benchmarks/
  experiments/{id}/
    experiment.json               # Experiment metadata and status
    {variant_id}.toml             # Frozen pipeline config snapshot
  suites/{id}/
    suite.json                    # Test suite metadata
    scenarios/
      {scenario_id}.wav           # Scenario audio files
  results/
    {experiment_id}/
      results.json                # Full ExperimentResults JSON
    benchmark-multi-*.json        # Script-generated result snapshots
    benchmark-sequential-*.json
```

Configure the base directory in `orchestrator.toml`:

```toml
[orchestrator]
benchmarks_dir = "benchmarks/"   # default
```

---

## Audio Injection (Internal Details)

The runner injects audio via WebSocket using the `InjectAudio` control message:

```json
{
  "type": "InjectAudio",
  "source": "benchmarks/suites/{suite_id}/scenarios/greeting.wav",
  "scenario_id": "greeting",
  "experiment_id": "exp-001",
  "speed": 0.0
}
```

The worker reads the WAV, chunks it into 20 ms frames, and feeds them through
the pipeline at the requested speed. On completion, the server sends an
`injection_complete` data message with chunk/sample counts and audio duration.

For `text_prompt` scenarios, the runner sends a text message directly, bypassing
VAD and STT entirely.

---

## Troubleshooting

### Worker does not register

- Check that the orchestrator is running and reachable at the configured
  `[orchestrator] endpoint`
- Verify the worker's `debug_port` is not already in use (the worker will
  auto-increment to the next available port and warn, but verify the
  orchestrator endpoint still matches)
- Verify the pipeline's `transport.port` is not already in use (same
  auto-increment behavior applies)
- Check worker stderr logs (scripts save these as `worker-*-stderr.log`)

### Experiment stays in "running" forever

- The per-scenario timeout may be too low (increase `-TimeoutS`)
- A worker may have crashed - check its stderr log
- The LLM server may be unresponsive - verify with
  `curl http://localhost:8081/health`

### STT/TTS metrics differ between variants despite same local models

This happens when both workers run simultaneously and one variant uses a local
LLM (consumes GPU/CPU) while the other uses a cloud LLM. The local LLM's
resource usage slows down the shared STT and TTS inference.

**Fix:** Use `run-benchmark-sequential.ps1` to run one variant at a time.

### "Experiment is not in draft status" error

The run endpoint only accepts `draft` experiments for an unfiltered full run.
If the experiment is `complete` or `failed`, pass `?variants=<id>` to trigger a
partial rerun or `?force=true` to overwrite all variants.

### OpenAI API errors

- Verify `$env:OPENAI_API_KEY` is set
- Check rate limits and billing on your OpenAI account
- The benchmark script validates the key before starting

### Results JSON is empty or missing variant data

- Check that variant tags match worker tags exactly
- Verify workers were active when the experiment ran (`curl http://localhost:9090/api/v1/workers`)
- For sequential runs, each phase must complete before the worker is stopped
- The results JSON now includes diagnostic fields when things go wrong:
  - **`error_reason`** on each variant — set when no worker matched the
    variant's tags, e.g. `"no available worker matching variant tags [\"variant:openai\"] — check that a worker with these tags is running and registered"`
  - **`error_message`** on each scenario — set when repetitions failed, e.g.
    `"3/3 repetitions failed: WS connect timeout"`
  - **`repetitions_failed`** on each scenario — count of failed repetitions
    (partial failures are still counted even if the scenario passes overall)

### Transient connection failures

The benchmark runner retries transient WebSocket errors (connect timeout,
connection refused, session-ready timeout) up to **2 times** with a 2-second
delay between attempts. Non-transient errors (server-side, protocol) fail
immediately. If all retries are exhausted the repetition is marked as failed
and the error is captured in the results JSON.

---

## Synthetic Caller (S2S Multi-Turn)

The `synthetic_caller` scenario kind drives the pipeline with a *real* S2S
model on the caller side (OpenAI Realtime, Gemini Live) so multi-turn
behaviour, barge-in policy, and persona drift can be exercised without a
human in the loop. A `mock` backend replays a scripted file for no-API
smoke tests.

### Feature gates and environment

- Build with `--features synthetic-caller` to pull in the live backends.
  Without the feature, `mock` still works and the live kinds return a
  clear error.
- Set `OPENAI_API_KEY` for `caller.kind = "openai_realtime"`.
- Set `GEMINI_API_KEY` for `caller.kind = "gemini_live"`.

### Bridge modes

`bridge` controls how the runner connects the pipeline and the caller.

| mode | direction | what it exercises | recording |
|------|-----------|-------------------|-----------|
| `text` (default) | caller transcript → pipeline `text` turn; pipeline LLM text → caller `push_text` | VAD/LLM/TTS only; pipeline STT bypassed | pipeline-side `tts_frag_*.wav` only (no mic input) |
| `audio` | caller PCM → pipeline binary frames (with `FLAG_END_OF_UTTERANCE` on the final frame); pipeline TTS binary → `caller.push_audio` | full VAD/STT/LLM/TTS round-trip | both `mic_raw.wav` (caller speech, downsampled to 16 kHz) and `tts_frag_*.wav` are captured automatically by the worker's per-session audit and served by the admin UI as `/api/v1/sessions/<id>/audio/{in,out}` |

`text` keeps API costs bounded and gives deterministic transcripts;
`audio` is what you want when validating STT / barge-in / endpointing
end-to-end.

### Persona templates

Set `persona_template` to one of the canned names instead of (or in
addition to) `persona`. When both are set the template is concatenated
first, with the free-form `persona` appended.

Available templates: `polite_time`, `impatient_refund`,
`curious_greeter`, `barge_in_tester`. See
[`voice-pipes-bench/src/synthetic_caller/prompts.rs`](../../voice-pipes-bench/src/synthetic_caller/prompts.rs)
for the canonical list and their bodies.

### Cost guardrails

Every synthetic-caller scenario **must** specify
`stop.max_turns` and `stop.max_duration_s`. Optional
`stop.max_caller_silence_s` aborts the scenario early if the caller
stays silent for that many seconds after a pipeline turn. The runner
also exposes `barge_in.max_consecutive_failures` to stop the scenario
if forced interrupts repeatedly fail.

### Example suites

- [`voice-pipes-bench/examples/benchmark/synthetic-caller-openai.json`](../../voice-pipes-bench/examples/benchmark/synthetic-caller-openai.json) — polite caller
  asks the time over OpenAI Realtime.
- [`voice-pipes-bench/examples/benchmark/synthetic-caller-gemini.json`](../../voice-pipes-bench/examples/benchmark/synthetic-caller-gemini.json) — two scenarios:
  text-bridge `polite_time` and audio-bridge `impatient_refund`.
- [`voice-pipes-bench/examples/benchmark/synthetic-caller-mock.json`](../../voice-pipes-bench/examples/benchmark/synthetic-caller-mock.json) — deterministic offline smoke test using `MockCaller`.

### Live gated test

`voice-pipes-bench/tests/synthetic_caller_openai_live.rs` runs a real OpenAI Realtime
session against a local worker. It is `#[ignore]` by default; opt in
with `RUN_SYNTHETIC_CALLER=1` and a valid `OPENAI_API_KEY`.

---

## What's Next

- **LLM-as-judge** — use a strong LLM to evaluate `response_intent`, relevance,
  and naturalness beyond deterministic substring/WER checks
- **Regression gating** — CI/deployment gate if P95 latency regresses or quality
  score drops below a threshold


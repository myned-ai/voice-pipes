# TTS Benchmarking — Implementation Reference

Status: active
Owner: Backend + QA
Last reviewed: 2026-05-11
Source of truth: yes (implementation reference)

Status: complete (Layers 0–4 + CI smoke).

This document describes **what shipped** and **how to exercise it manually**.
For the design rationale, see the plan.

---

## 1  What was implemented

### 1.1 Pipeline metrics ([voice-pipes/src/pipeline/](../../voice-pipes/src/pipeline/))

`TurnMetrics` carries the following TTS-specific fields, populated by the
existing audit path (no double-measurement):

| Field | Meaning |
|---|---|
| `tts_synthesis_ms` | Wall time spent inside the TTS provider for the turn |
| `tts_audio_duration_ms` | Duration of the produced audio |
| `tts_sample_rate` | Sample rate of the produced audio |
| `tts_chars` | Character count of the synthesised text |
| `tts_rtf` | Real-time factor = `synthesis_ms / audio_duration_ms` |
| `tts_provider` | Provider name from the registry (`piper`, `pocket-http`, …) |
| `tts_voice` | Voice id when available |
| `tts_cold_start` | `true` for the first synthesis on a session/worker |
| `tts_error` | Failure message; absent on success |

Cold-start tracking is a `Arc<AtomicBool>` shared between the per-session
pipeline turns and the new TTS-only handler. Provider failures keep the
flag set so the next attempt is still observed as cold.

### 1.2 Wire schema ([voice-pipes/src/transport/mod.rs](../../voice-pipes/src/transport/mod.rs))

New control frames (serde-tagged enums; backward-compatible with
existing payloads):

```rust
ClientControl::TtsOnlySynthesize { id, text, language, voice }
ServerControl::TtsOnlyResult     { id, metrics: TtsOnlyMetrics, audio_ref: AudioRef }
ServerControl::TtsOnlyError      { id, error: String }
```

`TtsOnlyMetrics` is `{ tts: TtsSynthesisMetrics, voice }` where
`TtsSynthesisMetrics` is the shared sub-struct also nested under
`ServerControl::TurnMetrics.tts`. Its fields are `ttfb_ms`,
`synthesis_ms`, `audio_duration_ms`, `sample_rate`, `chars`, `rtf`,
`provider`, `cold_start`, `error` (all optional). On the wire,
`TtsOnlyMetrics` flattens `tts` so the JSON shape stays
`{ ttfb_ms, synthesis_ms, …, voice }`. `AudioRef` points at a WAV on
disk so the benchmark runner can score it without re-streaming bytes.

### 1.3 Worker handler ([voice-pipes/src/transport/serve/runner/mod.rs](../../voice-pipes/src/transport/serve/runner/mod.rs))

`handle_tts_only` is dispatched from the WS select loop:

1. Swap `tts_cold_start` (atomic AcqRel).
2. Run `synthesize_stream` (with the request's hints), collecting frames into a `Vec<i16>`,
   tracking TTFB on the first non-empty frame.
3. Compute `audio_duration_ms` and RTF from the sample count.
4. Write WAV to
   `<captures_dir>/<session_id>/tts-only/<request_id>.wav`
   (falling back to OS temp when `captures_dir` is unset).
5. Emit `TtsOnlyResult` or `TtsOnlyError`. On no-audio failure, restore
   the cold flag via `compare_exchange`.

Bridge mode rejects `TtsOnlySynthesize` with an explicit error so match
exhaustiveness is preserved without changing direct-mode semantics.

### 1.4 Benchmark runner ([voice-pipes-bench/src/runner/](../../voice-pipes-bench/src/runner/))

New `ScenarioInput` variant `TtsOnlyCorpus { entries }` triggers
`run_tts_only_scenario`, which:

1. Opens one WS per scenario, sends `Configure` (tags `["benchmark",
   "tts_only"]`), waits for `SessionReady`.
2. Sends one `tts_only_synthesize` per `TtsCorpusEntry`, awaits the
   matching response by `id`.
3. Optionally runs `apply_tts_scoring` (when `RunnerConfig.tts_scorer`
   is set) against each persisted WAV.
4. Aggregates per-entry outcomes via `aggregate_tts_only_outcomes`.

The aggregator surfaces every field defined on `ScenarioMetrics` /
`VariantSummary`:

| `VariantSummary` field | Source |
|---|---|
| `avg_tts_ttfb_ms`, `p50/p90/p99_tts_ttfb_ms` | per-entry `ttfb_ms` |
| `avg_tts_synthesis_ms` | mean of `synthesis_ms` |
| `cold_tts_synthesis_ms` / `warm_tts_synthesis_ms` | split by `cold_start` flag |
| `avg_tts_audio_duration_s` | mean of `audio_duration_ms / 1000` |
| `avg_tts_rtf` | mean of `rtf` |
| `avg_tts_chars_per_second` | `chars / audio_duration_s` |
| `tts_failure_rate` | `failures / entries` |
| `avg_wer`, `avg_quality_score` | from scorer (Section 1.5) |

### 1.5 Round-trip WER scorer ([voice-pipes-bench/src/scoring.rs](../../voice-pipes-bench/src/scoring.rs))

```rust
pub trait TtsScorer {
    async fn score(&self, expected: &str, audio_path: &Path) -> Result<TtsScore>;
}

pub struct SttRoundTripScorer { /* wraps Arc<dyn SttProvider> */ }
pub struct TtsScore { wer, cer, transcript }
```

`SttRoundTripScorer` reads the persisted WAV, runs
`SttProvider::transcribe`, and computes WER/CER via embedded
Levenshtein-on-tokens (case-insensitive). Plug in any registered STT
provider via `RunnerConfig.tts_scorer = Some(Arc::new(SttRoundTripScorer::new(stt)))`.

The aggregator folds scores into `ScenarioScoring { wer: avg, cer: avg,
quality_score: 1 - avg_wer }` only when at least one entry was scored.

### 1.6 Benchmark text corpus ([voice-pipes-bench/examples/benchmark/tts-corpus.toml](../../voice-pipes-bench/examples/benchmark/tts-corpus.toml))

Stratified text fixtures (5 short / 5 medium / 5 long across plain,
numerics, punctuation, mixed). `language = "en-US"` in v1; the
`language` field on `TtsCorpusEntry` is reserved for future
multilingual corpora.

### 1.7 Frontend

| File | Purpose |
|---|---|
| [TtsPanel.tsx](../../voice-pipes-admin/frontend/src/components/benchmark/TtsPanel.tsx) | Renders P50/P90/P99 TTFB, avg synth, RTF, audio dur, chars/sec, cold/warm split, failure %. Hides itself when no fields populated. |
| [VariantCard.tsx](../../voice-pipes-admin/frontend/src/components/benchmark/VariantCard.tsx) | Hosts `TtsPanel`. WER badge turns red + `(regression)` for `wer > 0.25`. |
| [BenchmarkCompare.tsx](../../voice-pipes-admin/frontend/src/pages/benchmarks/BenchmarkCompare.tsx) | Delta table includes `P50/P90 TTS TTFB`, `Avg/Cold/Warm TTS synth`. Empty rows hidden. |
| [types.ts](../../voice-pipes-admin/frontend/src/components/benchmark/types.ts) | `VariantSummary` extended with the 10 new TTS fields. |

### 1.8 CI ([.github/workflows/tts-bench-smoke.yml](../../.github/workflows/tts-bench-smoke.yml))

Two path-gated jobs per push/PR:

- **rust-smoke** — `cargo fmt --check`, build the CLI with `tts-pocket,tts-piper`, `cargo test --lib benchmark`, `cargo test --test tts_benchmark_smoke`, transport codec tests.
- **frontend-smoke** — `npm ci`, vitest for `TtsPanel.test.tsx`, eslint on the touched components.

Heavy variants (Pocket-TTS sidecar, Piper voice models, full worker spawn) are intentionally deferred to a future nightly job with cached weights.

### 1.9 Test inventory

| Suite | Count | Location |
|---|---|---|
| Benchmark lib (incl. scoring + TTS aggregator) | 122 | `voice-pipes-bench/src/runner/`, `voice-pipes-bench/src/scoring.rs`, `voice-pipes-bench/src/store.rs` |
| Transport codec (incl. `tts_only_*` round-trips) | 13 | `voice-pipes/src/transport/ws/codec.rs` |
| Corpus smoke | 2 | `voice-pipes-bench/tests/tts_benchmark_smoke.rs` |
| Frontend (`TtsPanel` + `VariantCard`) | 7 | `voice-pipes-admin/frontend/src/components/benchmark/__tests__/TtsPanel.test.tsx` |

---

## 2  Manual testing — the fast path

Goal: emit one `tts_only_synthesize` against a running worker and
observe the metrics + persisted WAV.

No orchestrator required for this section — `wscat` connects to the
worker directly.

### 2.1 Bring up a worker with TTS

The pipeline config is passed via the **top-level** `--config` flag, and
the worker subcommand is `serve` (not `worker`):

```pwsh
cd voice-pipes
cargo run --release -p voice-pipes-cli --features tts-piper -- --config examples/bench-piper.toml serve
```

> **Windows note:** `cargo run` overwrites `target\release\voice-pipes.exe` on
> every rebuild. If the binary is already running that fails with *Access is
> denied*. Build once and invoke the binary directly instead — see §3.2 for
> the pattern.

The worker will print its WS bind address. With the bundled
[`bench-piper.toml`](../../voice-pipes/examples/bench-piper.toml) the
transport listens on `127.0.0.1:8802` and the WS endpoint is `/ws`.
Note the captures directory from the `[pipeline.audit]` section — TTS
WAVs land at `<captures_dir>/<session_id>/tts-only/<id>.wav`. If
`captures_dir` is unset, files go to
`%TEMP%/voice-pipes-tts-only/<session_id>/tts-only/<id>.wav`.

### 2.2 Drive a single synthesis with `wscat`

```pwsh
npx wscat -c ws://127.0.0.1:8802/ws
```

The worker will immediately push a `session_ready` control frame with
the negotiated audio format and the `session_id`. Once you see it,
paste the synthesis request:

```json
{"type":"control","payload":{"action":"tts_only_synthesize","id":"manual-001","text":"Hello there, this is a manual test of the TTS only path.","language":"en"}}
```

You should receive a `tts_only_result` like:

```json
{
  "type": "control",
  "payload": {
    "action": "tts_only_result",
    "id": "manual-001",
    "metrics": {
      "ttfb_ms": 84,
      "synthesis_ms": 312,
      "audio_duration_ms": 3580,
      "sample_rate": 22050,
      "chars": 58,
      "rtf": 0.087,
      "provider": "piper",
      "cold_start": true,
      "voice": "en_US-lessac-medium"
    },
    "audio_ref": {
      "path": "C:\\Users\\<you>\\AppData\\Local\\Temp\\voice-pipes-tts-only\\<sid>\\tts-only\\manual-001.wav",
      "format": "wav",
      "sample_rate": 22050,
      "channels": 1,
      "duration_ms": 3580
    }
  }
}
```

Open the WAV in any audio player to verify the synthesised speech.

A second request on the same session should report `cold_start: false`.

### 2.3 Trigger an error path

Send a payload that the provider rejects (empty text against most
providers works):

```json
{"type":"control","payload":{"action":"tts_only_synthesize","id":"manual-002","text":""}}
```

Expect a `tts_only_error` with the provider message. Then issue
`manual-003` with valid text and confirm `cold_start: true` is
preserved (the failed attempt did not warm the worker).

---

## 3  Manual testing — full benchmark flow

Goal: run a `TtsOnlyCorpus` scenario across multiple variants and
inspect the rolled-up summary.

### 3.1 Build once

On Windows, `cargo run` holds a lock on `voice-pipes.exe` while it is
running. Launching a second `cargo run` immediately fails with *Access
is denied* when Cargo tries to overwrite the binary. Build with both
feature flags combined so a single binary covers all variants:

```pwsh
cd voice-pipes
cargo build --release -p voice-pipes-cli --features tts-piper,tts-pocket
```

### 3.2 Start the orchestrator

```pwsh
# Terminal O
.\target\release\voice-pipes.exe orchestrate
```

(Listens on `http://127.0.0.1:9090` by default. Pass
`--orchestrator-config <path>` to override.)

### 3.3 Start the worker(s)

Invoke the pre-built binary directly in separate terminals — no
recompile, no file-lock conflict:

```pwsh
# Terminal A — Pocket-TTS sidecar
uvx pocket-tts serve --host 127.0.0.1 --port 8000

# Terminal B — Piper worker
.\target\release\voice-pipes.exe `
    --config examples/bench-piper.toml serve `
    --worker-config worker-bench-piper.toml

# Terminal C — Pocket worker
.\target\release\voice-pipes.exe `
    --config examples/bench-pocket-http.toml serve `
    --worker-config worker-bench-pocket.toml
```

Both workers register with the orchestrator and advertise their
`tts:<provider>` tags.

### 3.3 Create a TTS-only suite

Build a JSON body from the bundled corpus. The shape of `scenarios[i]`
is `TestScenario` with `input = { type: "tts_only_corpus", entries: [...] }`.
Smallest possible body:

```json
{
  "name": "tts-only-smoke",
  "scenarios": [
    {
      "id": "tts-corpus",
      "name": "tts-corpus",
      "input": {
        "type": "tts_only_corpus",
        "entries": [
          { "id": "short-plain-01", "text": "Hello there, how can I help you today?", "bucket": "short", "class": "plain" },
          { "id": "short-numerics-01", "text": "Your order number is 4 8 2 1 9 7.", "bucket": "short", "class": "numerics" }
        ]
      }
    }
  ]
}
```

POST it:

```pwsh
$body = Get-Content suite.json -Raw
$resp  = Invoke-RestMethod -Method Post -Uri http://127.0.0.1:9090/api/v1/benchmark/suites `
                            -ContentType 'application/json' -Body $body
$suiteId = $resp.data.id
```

### 3.4 Create + run an experiment

```pwsh
$expBody = @{
  name = "piper-vs-pocket smoke"
  test_suite_id = $suiteId
  variants = @(
    @{ id = "piper";  name = "piper";  config_path = "examples/bench-piper.toml";       config_hash = ""; tags = @("tts:piper") },
    @{ id = "pocket"; name = "pocket"; config_path = "examples/bench-pocket-http.toml"; config_hash = ""; tags = @("tts:pocket-http") }
  )
} | ConvertTo-Json -Depth 5

$exp = Invoke-RestMethod -Method Post -Uri http://127.0.0.1:9090/api/v1/benchmark/experiments `
                          -ContentType 'application/json' -Body $expBody

Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:9090/api/v1/benchmark/experiments/$($exp.data.id)/run"
```

Poll until `status == "complete"`:

```pwsh
do {
  Start-Sleep -Seconds 2
  $r = Invoke-RestMethod -Uri "http://127.0.0.1:9090/api/v1/benchmark/experiments/$($exp.data.id)"
  $r.data.status
} while ($r.data.status -in @("draft","running"))
```

### 3.5 Inspect results

```pwsh
$results = Invoke-RestMethod -Uri "http://127.0.0.1:9090/api/v1/benchmark/experiments/$($exp.data.id)/results"
$results.data.variant_results | ForEach-Object {
  "{0,-10} synth={1,5}ms  rtf={2:F2}  cold={3,4}ms  warm={4,4}ms  fail={5:P0}" -f `
    $_.variant_name, `
    [int]$_.summary.avg_tts_synthesis_ms, `
    $_.summary.avg_tts_rtf, `
    [int]$_.summary.cold_tts_synthesis_ms, `
    [int]$_.summary.warm_tts_synthesis_ms, `
    $_.summary.tts_failure_rate
}
```

Then open the admin UI at `http://127.0.0.1:5173/benchmarks/<exp_id>`
and verify:
- `VariantCard` shows the **TTS** section with all rows populated.
- `BenchmarkCompare` (when comparing two experiments) lists the new
  `P50/P90 TTS TTFB`, `Avg/Cold/Warm TTS synth` rows.
- Files exist under `<captures_dir>/<session_id>/tts-only/*.wav` for
  every entry that succeeded.

### 3.6 Optional: round-trip WER scoring

The default API path does not enable scoring (`tts_scorer = None`). To
exercise it manually you currently need to construct a
`RunnerConfig` in code (e.g. a one-off binary or test):

```rust
let stt: Arc<dyn SttProvider> = ...;        // any registered provider
let scorer = Arc::new(SttRoundTripScorer::new(stt));
let config = RunnerConfig { tts_scorer: Some(scorer), ..Default::default() };
run_experiment(&store, &workers, &experiment_id, &config).await?;
```

After completion, `variant_results[*].scenarios[*].scoring.wer` and
`variant_results[*].summary.avg_wer` will be populated, and
`VariantCard` will render the WER badge (red + `(regression)` when
`wer > 0.25`).

---

## 4  Manual testing — frontend in isolation

If you only need to eyeball the UI without spinning up a worker, the
`TtsPanel` component covers three fixture shapes that are already used
by the vitest suite:

```pwsh
cd voice-pipes-admin/frontend
npx vitest run src/components/benchmark/__tests__/TtsPanel.test.tsx
```

To probe it interactively, edit any benchmark JSON returned by the API
and inject the new fields into a variant's `summary`, then refresh the
detail page.

---

## 5  Troubleshooting

| Symptom | Likely cause |
|---|---|
| `tts_only_error: tts_only_synthesize not supported in bridge mode` | The session is using the in-process bridge transport; switch to a real WS worker. |
| Every entry returns `cold_start: true` | Each entry uses a fresh worker session; expected when the runner opens one WS per scenario but unexpected within the same scenario. |
| `audio_ref.path` does not exist | `captures_dir` was unset and the OS-temp fallback was cleared between request and inspection. Set `[pipeline.audit] captures_dir = "..."` in the worker config. |
| Frontend `TtsPanel` does not appear | All TTS fields on the variant summary are `null`/missing — confirm the scenario was a `TtsOnlyCorpus` and the worker reported metrics. |
| WER badge missing | `tts_scorer` was not configured; see §3.6. |

---

## 6  Deferred / explicitly out-of-scope

- Concrete `WhisperRoundTripScorer` (no Whisper STT registered in-tree
  yet; `SttRoundTripScorer` accepts any `SttProvider`).
- API surface to configure `tts_scorer` from the experiment POST body.
- Heavy nightly CI with cached Piper / Pocket weights and a real
  end-to-end run.
- UTMOS / DNSMOS subjective-quality scoring.
- Multilingual corpora (`language` field is already plumbed).

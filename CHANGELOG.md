# Changelog

All notable changes to Voice Pipes are documented here. The format follows
[Keep a Changelog 1.1](https://keepachangelog.com/en/1.1.0/) and the project
uses [Semantic Versioning](https://semver.org/). Until 1.0, minor versions may
contain breaking changes; they are called out under **Changed** with a
migration note.

## [Unreleased]

## [0.1.0] - 2026-11-11

The initial release of Voice Pipes: an ultra-low-latency, modular conversational
voice pipeline runtime written in safe Rust.

### Added

- Contiguous engine-measured telescoping stage latencies for cascade turns:
  `llm.llm_wait_ms` (`utterance.committed_at` to first LLM fragment),
  `tts.tts_wait_ms` (first LLM fragment to first raw TTS audio chunk),
  `tts.output_wait_ms` (first raw TTS audio chunk to first emitted audio frame),
  and `tts.commit_to_first_audio_ms` (total post-commit pipeline wait, satisfying
  `llm_wait_ms + tts_wait_ms + output_wait_ms == commit_to_first_audio_ms` within 1 ms).
- True speech-end anchor in `endpointing_ms`: VAD tracks `last_speech_at` on speech
  frames so trailing silence hangover frames forwarded to STT do not overwrite it.
- `measurement.stages_disagree` anomaly emitted by session report when telescoping
  stages disagree with `commit_to_first_audio_ms` by > 50 ms and > 5%.
- `tts.stage_skipped_reason = "speculative_accept"` on speculative accepted turns.

### Changed

- Workspace layout: moved all 20 provider plugin crates from the repository root
  into stage-grouped directories under `plugins/<stage>/<name>` (`plugins/stt/*`,
  `plugins/tts/*`, `plugins/llm/*`, `plugins/s2s/*`, `plugins/turn/*`,
  `plugins/vad/*`) while keeping the 7 core/shared crates (`voice-pipes`,
  `voice-pipes-sdk`, `voice-pipes-cli`, `voice-pipes-onnx`, `voice-pipes-net`,
  `voice-pipes-bench`, `voice-pipes-gym`) and `voice-pipes-admin` at the root.
  Every plugin retains its Cargo package name (`voice-pipes-<stage>-<name>`),
  so Cargo features, crate imports, `-p` flags, and `RUST_LOG` targets are
  unchanged. Consolidated helper scripts into the root `scripts/` directory,
  grouped benchmark/dev example configs into `voice-pipes/examples/bench/` and
  `voice-pipes/examples/dev/`, and removed the stale nested
  `voice-pipes/Cargo.lock` (see `docs/explanation/decisions/workspace-layout.md`).

- `tts.ttfb_ms`: deprecated as an overloaded commit-anchored wait. It is now
  strictly TTS's own wait (`tts_wait_ms` integer-ms alias) and is never overwritten
  by the transport runner with commit-to-first-audio. Use `commit_to_first_audio_ms`
  for the total post-commit wait.

- `voice-pipes models fetch [NAME...]` and `models list`: download model
  files to their default locations without the wizard's questions. With no
  names it fetches what `examples/minimal.toml` runs on, which is now the
  complete local agent (Silero, Parakeet, SmolLM2-360M in process, Piper);
  the source quickstart is build, fetch, `live`.
- The wizard offers SmolLM2-360M (first), 1.7B and 135M for the in-process
  LLM. 135M rambled; 1.7B answers at about half 360M's speed on a CPU.
- Admin console, native runs: `voice-pipes-admin/start-native.ps1` (Windows)
  and `start-native.sh` (Linux) set up and start the backend and the
  frontend from the locked dependencies, generate the sign-in token, and
  point the backend at the local orchestrator and captures. The backend now
  installs from a full lock (`requirements.txt`: every package pinned with
  its hashes, from `requirements.in`), runs on Python 3.12 only (the image
  moved from 3.11, which CI never tested) and refuses another version at
  start; the frontend admits Node 22.13+ and 24, both tested in CI, and
  `npm` refuses others.
- `launch-stack.ps1 -SkipWizard` / `launch-stack.sh --skip-wizard` start the
  stack on the configuration already written. Run outside the launcher, the
  wizard now ends by naming that command for the platform; run by the
  launcher, it prints no next step.
- Plugin SDK: `RecognitionRequest::slot_wait` (`with_slot_wait`), how long
  a stream may wait for a free inference slot before being refused. The
  engine sets a minute on the stream that replaces one it gave up on, since
  the replaced stream's last run holds the slot until the model returns;
  before, the replacement was refused every few seconds until the stall
  cleared and the session lost every utterance in between. Nemotron 3.5,
  Parakeet and Moonshine honour it.
- `[pipeline] keep_warm_interval_s` (default 300): an idle worker runs its
  recogniser once per interval so the model stays resident. Left alone on a
  host short of memory, a worker's model pages were evicted and the first
  utterance after the quiet spell waited for them longer than the
  pipeline waits for a final, so the caller got nothing. Recognisers warmed
  once per process take part; `0` disables it.
- `openai-compat`: `[text_processor.extra_body]` sends fields a server takes
  beyond the common API as given (llama-server's `chat_template_kwargs`);
  the keys the processor's own options set are refused there. A thinking
  model such as Gemma 4 otherwise spends every turn's token budget on
  reasoning the pipeline never speaks; the shipped Nemotron 3.5 pipeline
  turns thinking off and declares the client tools, and Gemma 4 E4B on
  llama-server calls them from speech.
- Parakeet and Piper report their execution target (`device_info`), the
  same report the other ONNX plugins give, so the component check and the
  provider guides' device line hold for them too.
- Streaming voice pipeline runtime in Rust: VAD → STT → turn detection → text
  processor chain → TTS, with a dedicated barge-in monitor and word-exact
  playback truncation.
- Local providers that run with no cloud keys: Parakeet TDT, Moonshine and
  Nemotron STT; Piper, Kokoro and VibeVoice TTS; quantized GGUF LLMs via
  candle or any OpenAI-compatible endpoint; Silero VAD.
- Cloud providers: AssemblyAI and Google Cloud STT v2 (Chirp 3) streaming
  STT; OpenAI, Gemini and ElevenLabs TTS; OpenAI Realtime, Gemini Live and
  ElevenLabs Agents as speech-to-speech backends with tool calling,
  including client-fulfilled tools.
- `voice-pipes-sdk`: semver-disciplined plugin boundary for external STT,
  TTS and text-processor providers.
- Per-turn, per-component latency metrics, full audio and event audit trail,
  OpenTelemetry export, and streaming-phase throughput metrics.
- Off-path audio annotators (blendshapes, visemes, phonemes) over a
  streaming gRPC sidecar.
- Orchestrator with worker registry, heartbeats, least-connections dispatch
  and drain; single-worker standalone mode.
- `voice-pipes-bench`: A/B pipeline benchmarking and a synthetic caller for
  automated voice testing with deterministic scoring.
- Web admin console (React + FastAPI): live playground with karaoke
  captions, session explorer with per-turn breakdowns, benchmark results.
- One-command Docker stack launcher with an interactive configuration
  wizard and on-demand model download.
- Session report: a tool-invocation turn's wait is measured from speech
  end to the tool call's dispatch and raised as `turn.unanswered_wait`
  against the model; a model that sat on a clean request for twenty
  seconds was graded as a slow client tool before.
- Protocol: `utterance_dropped` (server to client) when the user spoke and
  no turn came of it (`empty_transcript`, `too_short`, `stt_timeout`), so
  speech that interrupted the agent and was not recognised no longer looks
  like an agent that refused to answer. The Playground shows it as a
  system line; the session report raises `stt.interruption_unheard` for
  the first drop after a server-side barge-in; a second utterance dropped
  under the same id keeps its own recording in the capture, and the
  console's session detail shows every dropped utterance as a "Heard, not
  recognised" card with its recording.
- Session report and console: `output.pacer_wait_ms`, the realtime pacer's
  share of the time between receiving audio and handing it to the transport,
  reported apart from `chain_overhead_ms`, which now holds only the
  post-processors' summed call time (0 when none ran). On a pipeline with no
  post-processor the pacer's wait (4920 ms on a 6.4 s reply) was labelled
  the post-processors' share and sent a reader looking for a slow processor
  that did not exist; a slow emit is blamed on the post-audio chain only on
  a turn that ran a processor. The Playground shows a Pacer pill, and the
  analytics rollup is recomputed.
- `voice-pipes healthz --url` probes a health endpoint within a bound and
  exits by the answer. The engine, CUDA, admin backend and admin frontend
  images carry a `HEALTHCHECK` built on it (or on the smallest probe the
  image has), compose starts every worker only after a healthy orchestrator
  and the frontend after a healthy backend, and the frontend image builds on
  `node:22-alpine` (Node 20 is past its end of life).
- Workers and the orchestrator shut down gracefully on Ctrl-C or SIGTERM:
  every open session gets a `closing` frame (reason `worker_shutdown`),
  `/readyz` answers 503, the listeners stop accepting, sessions have ten
  seconds to close, and the worker deregisters at once through the new
  `POST /internal/workers/deregister`, so the orchestrator stops dispatching
  to it instead of waiting out the heartbeat. A client used to see a bare
  socket close and a capture could miss its last write. A transport listener
  that fails while heartbeats continue ends the process with an error so a
  supervisor restarts it; the never-set `fatal` flag behind `/healthz` is
  gone.
- `deploy/docker/docker-compose.noswap.yml` keeps voice workers out of swap:
  every service gets a memory limit equal to its memory-plus-swap limit
  (sized by `VP_MEM_<SERVICE>`, defaulting to measured footprints with
  headroom) and llama-server locks its weights in RAM. An idle worker's
  models are what the kernel swaps out first, and the next caller paid for
  reading them back: a worker with 0.9 GB swapped answered its first turn
  in 2.9 to 3.3 s from the end of speech against 1.3 s warm, nearly all of
  it in speech recognition. `scripts/idle_soak.py` measures what idling
  costs on a host (first turn against a warm one, stage by stage, with the
  worker's swapped memory and page faults; `--squeeze-mb` swaps a worker
  out on purpose), `s2s_probe.py` can return every component's extras, and
  the deployment guide explains the cost, the override and how to size it.
- Plugin SDK 1.0: semver-stable, with a written stability policy
  (`docs/extending/sdk-stability.md`): what a minor release may add, the
  structs whose fields are complete, and the dependency versions the API
  exposes (`tokio-util` 0.7 and `async-trait` 0.1 for all of 1.x). The
  workspace's other crates share one version (`[workspace.package]`) and
  move in lockstep.
- Plugin SDK: `voice_pipes_sdk::audio::to_mono`, the one interleaved-to-mono
  downmix, reading a channel count of 0 as mono. Parakeet, Moonshine and
  English Nemotron use it; Parakeet's own copy panicked on a gRPC frame with
  no channel count.
- Plugin SDK: `voice_pipes_sdk::tts::recorded_unless_cancelled`, `recorded`
  for a synthesis a cancel token can stop: a stream cut short by a barge-in
  is not counted as a success or a latency sample. OpenAI, ElevenLabs and
  Pocket TTS stream through it, and the OpenAI-compatible LLM skips
  recording a cancelled turn the same way.
- Plugin SDK: the untyped option readers on `RealtimeConfig`
  (`option_str`, `option_bool`, `option_u64`) and `TurnConfig`
  (`option_str`, `option_f64`, `option_u64`, `whole_option`,
  `number_option`) are deprecated in favour of `options()`, the one typed
  reader every provider config already had; `option_bool` read the string
  `"true"` as unset. The three speech-to-speech providers and both turn
  detectors read through `options()`, and its wrong-type messages now name
  the accepted range. The deprecated methods keep working until 2.0.
- Plugin SDK: `SttConfig::metadata_descriptions`,
  `TtsConfig::metadata_descriptions`, `llm_metadata_descriptions` and
  `TextProcessorConfig::unknown_options` are deprecated: nothing read the
  first three (the console uses each provider's `param_schema`), and
  `options()` with `finish()` does what the last did.
- `voice_pipes_onnx::{dim, nth_output, ModelOutputError}` (feature
  `session`): a run's output by index and a reported length, as an error
  naming the session rather than a panic. Parakeet and English Nemotron use
  them instead of their own copies.
- Plugin SDK: `voice_pipes_sdk::audio::samples_in_ms`, the samples a
  duration holds at a rate, saturating on a nonsense duration. Parakeet and
  Moonshine use it instead of their own copies.
- Plugin SDK: `voice_pipes_sdk::options::as_f32`, a setting's `f64` at the
  `f32` a provider keeps, saturating to infinity. Parakeet, Moonshine, the
  local LLM, and OpenAI and ElevenLabs TTS use it instead of their own
  copies.
- Plugin SDK: `voice_pipes_sdk::audio::f32_to_i16`, a model's `f32` audio
  as 16-bit PCM, clipped rather than wrapped. Piper and Kokoro use it
  instead of their own copies.
- Plugin SDK: `InferenceMetrics::measured` (duration, passes and `rtfx` from
  one utterance's audio and inference time) and
  `InferenceMetrics::with_delay_floor_ms` (the algorithmic delay and the
  effective latency it implies). Parakeet, Moonshine and both Nemotrons
  build their metrics with them instead of their own copies.
- Plugin SDK: `voice_pipes_sdk::audio::i16_to_le_bytes`, 16-bit PCM as the
  little-endian bytes a network API takes. AssemblyAI, Google STT and the
  SDK's own `encode_pcm16_b64` use it instead of their own copies.
- Plugin SDK: `voice_pipes_sdk::s2s::codec::count_u32`, a count in a
  vendor's frame (token usage, rate limits) as a `u32`: 0 when absent or not
  a non-negative integer, saturating past `u32`. Gemini Live and OpenAI
  Realtime use it instead of their own `saturating_u32` copies.
- Plugin SDK: `voice_pipes_sdk::tts::WordGrouper`, character-level timings
  grouped into `WordBoundary`s (whitespace ends a word, punctuation stays
  on it, a word split across chunks is stitched). ElevenLabs TTS and the
  ElevenLabs Agents speech-to-speech provider use it instead of their own
  copies.
- Plugin SDK: `voice_pipes_sdk::audio::mono_f32_at`, PCM16 of any channel
  count and rate as mono `f32` at a model's rate. Parakeet, Moonshine and
  English Nemotron's batch `transcribe` used three copies of that prologue.
- Plugin SDK: `voice_pipes_sdk::tts::detail_event`, the `tts_detail` audit
  event in one shape (elapsed, chars, audio duration, rate, rtf, rtfx, plus
  a provider's own fields). All six TTS providers emit through it; Piper
  and Kokoro no longer round `elapsed_ms` to a tenth of a millisecond or
  the ratios to two decimals.
- Every crate is ready for crates.io: none is `publish = false`, each has
  its metadata, workspace dependencies carry the versions crates.io
  resolves them by, and the engine ships only what it builds from (its
  `include` list leaves the VibeVoice sidecar, deploy files and fixtures in
  the repository). `publish.yml` publishes with crates.io trusted
  publishing (no stored token): a `vX.Y.Z` tag publishes the workspace,
  a manual run the SDK. CI's *packages* job dry-runs `cargo publish
  --workspace` on every change. Nothing is published yet; the first
  release is by hand (`docs/contributing/release-process.md`).
- Plugin SDK 0.6: `SttError::busy` and `TtsError::busy` for a saturated
  engine pool, and `voice_pipes_sdk::audio::i16_to_f32`;
  `voice_pipes_onnx::argmax` for greedy decoding. Parakeet, Moonshine,
  Piper, Kokoro, Silero and both Nemotrons use them instead of their own
  copies.
- Plugin SDK 0.6: `VadFactory::can_run` (defaulted: when `validate` reports
  nothing), which `[vad] mode = "auto"` asks of the backend the binary
  names with `vad::register_auto_backend`; a VAD plugin may register one
  wizard entry, the `[vad]` settings the wizard writes for it.
- Plugin SDK 0.6: `voice_pipes_sdk::tts::{collect_stream, recorded,
  requested_voice, adopt_voice, voice_cell, VoiceCell}`, the pieces every
  hosted TTS provider wrote for itself: whole-utterance synthesis on top of
  streaming, latency and error counts on the component, the voice a request
  asks for, and adopting the parameter store's live voice cell. OpenAI,
  ElevenLabs, Pocket and Gemini TTS use them.
- Plugin SDK 0.6: `voice_pipes_sdk::options::ProviderOptions`, the one way
  to read a component's option table: typed, bounded readers that report
  every problem as a `ConfigError` naming `<section>.<key>`, and refuse
  any key nothing read (`UnknownKey`). Each stage config hands one out
  with `options()`, with the keys the engine itself reads accounted for.
- `voice_pipes::plugins::register_plugin(Plugin::<Stage>(factory),
  wizard_choices)` registers a plugin's factory and its setup-wizard entries
  in one step (the factory is built once). Every stage's factory can
  declare `aliases()` (SDK 0.6: TTS, text processor, VAD and turn detection
  gained them), and `lookup_vad_factory` joins the other stages' lookups.
- Plugin SDK 0.6: a speech-to-speech session outlives its connection.
  `RealtimeEvent::SessionResumable` carries a provider's resumption token,
  `SessionConfig::resume_token` hands the latest back on a reconnect, and
  `GoingAway` says a connection is closing; `CloseClass::Unresumable` marks
  a close that refused a token. The realtime runner resumes on every
  reconnect (and says so to the client), starts afresh when a token is
  refused, and on `GoingAway` moves the session to a resumed connection at
  the next quiet moment. `RealtimeEvent::FunctionCallsCancelled` withdraws
  pending tool calls; a server tool that finishes after its call was
  withdrawn has its result dropped. Gemini Live maps all four and turns
  resumption on in every session. `Secret` implements `PartialEq`.
- Plugin SDK 0.6: `TurnDetector::turn_ended` (defaulted), called whenever
  the pipeline ends a turn, whatever ended it. Smart Turn clears its
  window there, so the next turn is not judged on the last one's audio.
- `voice_pipes_net::HttpOptions::streaming(idle)` bounds a streaming body
  by the gap between reads instead of the whole response, and
  `voice_pipes_net::sensitive_header` builds a header value that tracing
  never prints.
- Plugin SDK 0.5: turn detectors. `voice_pipes_sdk::turn` carries the
  `TurnDetector` trait, the `TurnSignal`/`TurnDecision` types and
  `TurnDetectorFactory`, built from a `TurnConfig` (the mode, the silence
  timeout, and every other key of `[turn_detection]` as options). The
  engine registers detectors with
  `voice_pipes::stt::turn::register_turn_factory`, and the admin console
  shows the selected detector's own settings from its `param_schema`.
  `[turn_detection]` no longer has fixed fields for Smart Turn and VAP:
  their keys stay where they were (configs are unchanged) and each
  detector checks its own, refusing any it does not read, so a misspelt
  or leftover key is still an error, now reported by validation rather
  than at parse time. `build` takes the `BuildContext` (the session's
  trace sink reaches a detector through it), the built-in modes refuse an
  invalid config in `build` as well as in `validate`, and `TurnDecision`
  and `TurnSignal` are `#[non_exhaustive]`. `TurnConfig::whole_option` and
  `number_option` read a bounded number and tell a wrong type from a value
  out of range; `ParamDescriptor::restart_required` marks a setting the
  console shows but cannot change live. `voice-pipes inspect --kind turn`
  and `--kind realtime` list the registered detectors and speech-to-speech
  providers.
- One ONNX Runtime session policy for every plugin.
  `voice_pipes_onnx::open_session` (the crate's new `session` feature)
  applies the thread count, graph optimisation and execution provider in
  one place; the Nemotron, Nemotron 3.5, Kokoro, Smart Turn and VAP plugins
  open their sessions through it, and a workspace rule keeps any plugin
  from building one directly. A plugin's `ort-cuda` switches on
  `voice-pipes-onnx/cuda`. `ExecutionTarget::from_env()` follows
  `VP_ORT_EP`; CUDA asked for in a build without it runs on the CPU with a
  warning, as before. The turn detectors stay on the CPU by design and say
  so in their settings. `voice_pipes_onnx::mel` (feature `mel`) holds the
  log-mel pieces the local models share (Hann window, Slaney filterbank,
  power spectra over a per-process FFT plan); the two Nemotrons and Smart
  Turn keep only their own steps, and `rustfft` is one dependency instead
  of three. Nemotron 3.5's features are bit-identical; the English
  Nemotron's and Smart Turn's move by under 1e-3 and 3e-5, their window
  and filterbank now computed in f64 as the reference front ends do.
- The plugin crates' public API is what a binary or a test needs:
  `factory()`, `wizard_choices()`, the config type, and what the crate's
  own examples and tests (and the bench's VAP bake-off) drive. Every other
  module and type is private, a workspace rule refuses a new public module
  that is not listed with its reason, and CI builds the plugins' docs with
  broken links as errors. Making them private exposed dead code, removed:
  AssemblyAI's unsent configuration-update message and two wire fields
  nothing read, and accessors on the speech-to-speech protocols.
- Plugin SDK 0.5: speech-to-speech providers. The new `voice_pipes_sdk::s2s`
  module carries the provider contract: the `RealtimeProtocol` wire mapper,
  the `RealtimeEvent` vocabulary, the neutral session types, the base64
  PCM16 codec, and `RealtimeFactory`, built from a `RealtimeConfig` that
  holds only the provider's settings. What a session is for travels with
  the session: `SessionConfig::speaks_first` (a benchmark's caller opens
  the conversation; an agent waits) and `transcribe_input`. `SessionConfig`,
  `ToolSpec`, `ConnectInfo` and `RateLimitInfo` are `#[non_exhaustive]`
  with constructors, `RealtimeConfig`'s and `SessionConfig`'s `Debug`
  redact secret-named options, and the engine's realtime client refuses a
  tool call with oversized arguments however the provider built it.
  The engine resolves the credential and hands it over as
  `RealtimeConfig::api_key` (a `Secret`, with `api_key_var` naming where it
  looked), so no provider reads the environment for a key;
  `missing_key_error` reports a missing one. `build` takes the
  `BuildContext`, and `RealtimeEvent`, `Modalities`, `TurnDetection` and
  `CloseClass` are `#[non_exhaustive]`. The engine re-exports the SDK as
  `voice_pipes::sdk`. The engine registers providers with
  `voice_pipes::realtime::register_realtime_factory`; its old
  `voice_pipes::realtime::{event, protocol, codec}` paths re-export the SDK.
  The resampler moved to `voice_pipes_sdk::audio::resample` (re-exported
  at `voice_pipes::audio::resample`). The benchmark's synthetic callers
  and the `interactive` command build their protocol through the
  registered factory instead of naming a vendor type; the callers' default
  model is now the provider's own.
- Nemotron 3.5 ASR as a **plugin crate**. `voice-pipes-stt-nemotron35` runs
  NVIDIA's 40-locale cache-aware streaming recogniser through ONNX Runtime
  (`provider = "nemotron-3.5"`, CLI feature `stt-nemotron35`), with the
  locale chosen per session from what the client negotiates. It is built on
  `voice-pipes-sdk` and the new `voice-pipes-onnx` session-pool crate alone;
  the engine contains no code for it, and the plugin SDK guide now carries it
  as the worked example of adding a local model without touching the core.
  `scripts/download_nemotron35.py` fetches the pinned, hash-checked export;
  no NeMo, no token. Croatian is in the model's broad-coverage tier and it
  measured well behind Parakeet v3 and Chirp 3, so
  `pipeline-croatian-nemotron35.toml` sits beside those pipelines for
  comparison, not in their place.
- Plugin SDK 0.5: shared helpers for hosted providers. `audio::PcmFramer`
  turns streamed 16-bit PCM into fixed frames, `audio::WavStripper` removes
  a WAV header however it is chunked (bounded, metadata chunks skipped),
  `TtsError::from_http_status` maps an HTTP status, and
  `secrets::resolve_key` resolves a key from config or environment. The
  four REST TTS plugins dropped their own copies (three PCM framers, two WAV
  parsers, four error maps, four key lookups); a provider now stops reading
  a stream once nothing is listening, and a blank configured key falls back
  to the environment as validation already assumed.
- Plugin SDK 0.5: text processors reach the whole session. The new
  `voice_pipes_sdk::agent` module carries `ClientToolSpec` (a tool as the
  model sees it) and a `SessionHandle` through which a processor reports the
  client-side tool calls its model makes; the `TextProcessor` trait gains
  defaulted `bind_session`, `configure_client_tools`,
  `process_context_stream`, `append_assistant_message`, `inject_context`,
  `model_identity` and `device_info`. A plugin LLM can now drive client
  tools and agent-initiated turns like a built-in one.
  The wizard gains `WizardDeploy::DockerSidecarModel`: a preset that starts a
  local model server (llama-server, say) on a model the wizard downloads
  first, with the model's location going to the launcher, not the TOML.
- Plugin SDK 0.4: voice activity detection backends can be plugins.
  `voice_pipes_sdk::vad` adds `VadBackend` and `VadFactory`, registered with
  `voice_pipes::vad::register_vad_factory` and selected by `[vad] mode`
  like a built-in backend; backend-specific settings go under
  `[vad.options]`. Config validation now runs the selected VAD backend's
  own checks, as it does for every other stage.
- Plugins offer themselves in the setup wizard. An STT, TTS or text
  factory returns a `WizardChoice` (label, Cargo feature, provisioning, the
  settings to ask for with defaults) and `voice-pipes config-wizard` lists
  it after the built-in options of its stage. The wizard's provider tables
  hold only built-ins now; Nemotron 3.5 describes its own entry, which also
  fixes the download script its old entry pointed at.
- Plugin SDK 0.3: `SttProvider` gains defaulted `warm_up_scope`,
  `concurrent_capacity`, `model_identity` and `device_info`, so a plugin
  recogniser can be warmed once per process, have its pool size checked
  against the worker's concurrency, and appear in the component check and
  session report as a built-in one does. `RecognitionRequest.language` is now
  filled from the session's negotiated language for every recogniser.
- Anticipatory turn detection, **experimental**. `[turn_detection] mode = "vap"` (Cargo feature
  `turn-vap`) runs [Voice Activity Projection](https://github.com/ErikEkstedt/VAP),
  which predicts who will hold the floor over the next two seconds from the
  user's microphone and the agent's output *together*. Every other mode
  answers "has the turn ended", from one channel, and can only answer once a
  pause has been paid for; this one can be asked while the user is still
  talking, so the pipeline gets lead time instead of a verdict after the fact.
  `scripts/export-vap-onnx.py` exports the pretrained checkpoint; the context
  window is fixed at export time, and the guide carries the CPU cost per
  window so the accuracy/CPU trade is made with numbers. Marked experimental
  because what is unproven is the model, not the plumbing: the window and
  threshold are defaults chosen for cost, and the bundled checkpoint is
  English, trained on human-human telephone speech rather than the
  human-agent conversation a voice agent actually sees. The guide says what
  to measure, and how to adopt a multilingual checkpoint or fine-tune on your
  own recordings.
- Continuous session recording. `[pipeline.audit] record_session_audio` (off by
  default) writes one stereo `session_audio.wav` per session — channel 0 the
  user before the VAD, channel 1 the agent — on a single timeline, from both
  the cascade and speech-to-speech engines. The existing per-utterance audio is
  post-VAD and stored one file per side, so the silence between turns and the
  relative timing of the two speakers are not recoverable from it; turn-taking
  models and any evaluation of interruption handling need exactly that.
- Speech-recognition accuracy testing. `voice-pipes-bench`'s WER/CER
  scoring now has a corpus runner (`--test stt_corpus`): point it at a
  model and a JSON corpus of WAV files with reference transcripts and it
  prints per-entry and per-group error rates, failing over a configurable
  budget. The per-turn diagnostic only grades latency, so a provider that
  answers fast with the wrong words used to look healthy. The Parakeet
  integration tests also gained a streaming-versus-batch parity check: the
  pipeline only ever calls `transcribe_stream`, which no test covered.
  Both are `#[ignore]`d and take their model and corpus paths from the
  environment. The Parakeet provider guide documents the multilingual v3
  export and what the corpus scorer measured for it.
- `SttCapability::CumulativeFinals` in the plugin SDK.
- `SttError::ReconnectExhausted { attempts, reason }` in the plugin SDK (a
  permanent error), and the streaming contract documented on
  `SttProvider::transcribe_stream`: one call per session, `is_last` marks
  an utterance boundary, `segment_index` numbers utterances, return only
  on cancel, channel close or an unrecoverable error.
- Nemotron `pool_size` option.

### Changed

- The cargo-features reference's CLI table is generated from the CLI
  crate's manifest and checked for drift like the engine's; it now lists
  every feature (`stt-parakeet`, `tts-piper`, `vad-silero`, `ort-cuda` and
  the pass-throughs were missing).
- The docs checker resolves `file.md#anchor` links against the target's
  headings, so a renamed section fails CI instead of scrolling nowhere.
- Contributor docs corrected from the code: what CI runs (every job and
  the no-skip gate), the README's Linux build prerequisite, the env
  example's three auth tokens, and the provider guides' reconnect
  defaults, pool sizes and environment variables.
- Gemini Live closes turns sooner: the wizard and the shipped Gemini
  pipelines set `end_sensitivity = "high"` and `vad_silence_ms = 700` under
  `[realtime_provider.options]`. At Gemini's defaults a clean request
  followed by real silence went unanswered for 21 s, until the caller spoke
  again, and since the caller's transcript arrives with the turn's end the
  text appeared late too; with these every turn closes within about a
  second. The guide explains the two settings.
- Session grading: a speech-to-speech session's `ttfb_ms` and
  `endpointing_ms` targets default to 1200 ms, as the report already
  relaxes `ttft_ms` for cloud LLMs. A healthy Gemini Live session (warm TTFB
  around 1.25 s) graded `degraded` against the engine's generic 600 ms
  realtime budget and the cascade's 500 ms endpointing; it grades `fair`.
  A pipeline's `[grading]` and the console's `ADMIN_GRADING_*` still win,
  and the hand-written targets in the shipped speech-to-speech pipelines
  are gone since the default covers them.
- The engine asks providers for their facts instead of guessing them: the
  pipeline graph shows a speech-to-speech provider's or a recogniser's
  sample rate as unknown rather than 24 or 16 kHz matched from a vendor
  name, a realtime session with no negotiated rate falls back to the
  session's own emit rate instead of 24 kHz, the endpoint observer's label
  is `vp_` plus the VAD backend's name, and the engine's `pool_size` check
  is gone since every pool-owning plugin refuses 0 itself. A workspace rule
  refuses a provider's name or alias as a string literal in engine code.
- Admin backend: logging is configured once from `LOG_LEVEL` (default
  `INFO`; `httpx` stays at `WARNING` unless the level is `DEBUG`), so every
  module's lines are seen; the orchestrator probe printed to stdout while
  the rest logged into nothing.
- CI runs the admin console: an `admin-backend` job (`ruff check` under the
  backend's new `ruff.toml`, `pytest`) and an `admin-frontend` job (lint,
  type-check, the full vitest suite, both admin images built and the
  frontend's security headers checked). The backend's 221 ruff findings are
  fixed; swallowed failures that hid defects (the verdict, session metrics,
  the pipeline-event pass, issue persistence, the worker snapshot) now log
  a warning with the traceback. The golden health replay skips with its
  reason when no capture corpus holds its sessions.
- Every WebSocket the shared connector opens sets `TCP_NODELAY`, so a
  speech-to-speech uplink's 20 ms audio frames no longer each wait for the
  previous frame's acknowledgement.
- The SDK stability policy (`docs/extending/sdk-stability.md`) says where
  `voice-pipes-net` and `voice-pipes-onnx` stand: both follow Cargo's
  pre-1.0 rule (a plugin depends on `"0.1"`), the surface each offers is
  listed, and the policy says when they come under it. It told plugin
  authors to build against the SDK alone while the workspace rules require
  both.
- **`voice-pipes-onnx` 0.1 (breaking):** `argmax` returns `Option<usize>`,
  `None` for an empty or all-NaN slice; it returned 0, so a broken model
  output decoded as token 0 (Nemotron 3.5 read empty logits as a blank).
  Parakeet, Moonshine, Nemotron and Nemotron 3.5 now fail that inference
  instead. `SessionPool::new` returns `Result<Self, EmptyPool>` and refuses
  an empty set, which made every checkout wait out its deadline and report
  the pool exhausted. `PoolExhausted` states its wait in milliseconds
  (`waited 250 ms`, not `waited 0 s`). Migration: add `?` (mapping
  `EmptyPool` into your error) after `SessionPool::new`, and handle `None`
  from `argmax` as an inference error.
- **Plugins (SDK 1.0, breaking for struct literals):** `WordInfo`,
  `RecognitionRequest`, `SynthesisHints`, `TtsSessionConfig`,
  `HealthStatus`, `ComponentTelemetry`, `ModelIdentity`, `DeviceInfo`,
  `TextChunkingConfig`, `SessionOutcomeInfo` and `RenderedChoice` are
  `#[non_exhaustive]`, so 1.x can add fields to them. Build them with
  `new` or `Default` and the new `with_*` setters
  (`WordInfo::new(word, start, end).with_confidence(..)`,
  `RecognitionRequest::default().with_sample_rate(..)`,
  `SynthesisHints::default().with_speed(..)`,
  `TtsSessionConfig::new(session_id)`, `ModelIdentity::from_config(id)
  .with_version(..)`); reading their fields is unchanged.
- **Plugins (SDK 0.6):** a wizard entry's `menu_rank` orders it among the
  stage's built-in entries too, not only among plugins: built-ins rank 1,
  2, … in their own order and come before a plugin entry of the same rank,
  so every current menu is unchanged. What a pipeline with no `[stt]` or
  `[tts]` section runs is registered by the binary
  (`plugins::register_fallback`), so a provider that moves out of the engine
  can stay that default; it is separate from the wizard's `stage_default`
  preselection, which a linked cloud provider may hold.
- CLI features are named for their crates: `tts-pocket` (was
  `tts-pocket-http`), `llm-gguf` (was `text-llm`) and `llm-openai-compat`
  (was `text-openai-compat`). The old names stay as aliases, so existing
  builds keep working; the images, examples and docs use the new ones. The
  per-plugin `stt-moonshine-cuda`, `stt-nemotron-cuda` and
  `stt-nemotron35-cuda` are aliases of the plugin plus `ort-cuda`, which
  already puts every linked ONNX provider on CUDA.
- Images: the worker feature list is defined once in `docker-compose.yml`
  (a YAML anchor every worker service reuses) instead of in seven copies,
  and the CUDA image builds that same list with `cuda,ort-cuda` on top
  (`GPU_FEATURES` in `Dockerfile.cuda`), since `ort-cuda` already switches
  every linked ONNX provider onto the GPU. The per-plugin `-cuda` features
  stay as aliases. A workspace rule holds both Dockerfiles to the list.
- CLI: `--log-format`, `inspect --kind` and the `--format` flags take only
  their listed values, which `--help` shows; a wrong one is refused with the
  choices before anything runs (`--kind` keeps `text_processor`,
  `turn_detection` and `s2s` as aliases). A failing command logs its whole
  error chain and flushes telemetry before exiting, and `--help` and
  `--version` answer before plugins register, so a registration problem
  cannot hide them.
- `voice_pipes::plugins::WizardStage` is `Stage`, the one description of a
  stage: its config table and provider key, its wizard question and the
  features that fill it, its providers with their aliases, and the error
  for an unknown one. The wizard, `inspect` and validation all read it, so
  `inspect` now lists the aliases of TTS voices, text processors and turn
  detectors, and an unknown provider is reported the same way at every
  stage ("unknown TTS \"x\"; this build has [...]"). The wizard's
  built-in entries (Parakeet, Moonshine, Piper, the two turn modes) are
  `WizardChoice` values like a plugin's and are written the same way; a
  typed path is trimmed as every other answer is. Plugin SDK 0.6:
  `WizardDeploy::Registry { key }` names files the wizard's model registry
  already describes. Migration: rename `WizardStage` to `Stage`.
- Plugin SDK 0.6: every factory's `build` returns
  `voice_pipes_sdk::BuildError` instead of `anyhow::Error`, so the SDK's
  signatures no longer tie a plugin to anyhow's version.
  `BuildError::Config(errors)` keeps a refused config's problems (what
  `options::refused` now returns), and `BuildError::Failed` keeps any other
  cause; `?` converts an `anyhow::Error`, a `Vec<ConfigError>` and the
  stage errors. AssemblyAI and Google STT report a refused config as its
  problems instead of one joined line. Migration: change the return type;
  replace `anyhow::bail!(..)` in `build` with
  `return Err(BuildError::failed(..))`.
- Plugin SDK 0.6: the wizard and schema types a plugin builds are
  `#[non_exhaustive]`: `WizardChoice` (`new(label, deploy)` and setters;
  unset, an entry ranks `WizardChoice::DEFAULT_MENU_RANK`), `WizardModel`
  (`new`), `WizardDownload` (`with_alt_names`), `WizardField` (its
  constructors, as before), `ParamDescriptor` (`new(key, label,
  description, value_type)` and setters, beside the typed constructors),
  `ParamConstraints` (`Default` and setters) and `OnlyWhen` (`new`). The
  enums `ParamValueType`, `ParamValue`, `ParamError` and `WarmUpScope` are
  non-exhaustive too. Migration: replace struct literals with the
  constructors, and give a `match` on those enums a wildcard arm.
- Plugin SDK 0.6: `SttConfig`, `TtsConfig`, `TextProcessorConfig` and
  `VadConfig` are `#[non_exhaustive]`, so a field added later is not a
  break. Build one with `new(provider)` and the `with_*` setters (each
  takes the field's own type, as `RealtimeConfig`'s do); fields stay
  public to read and to change on a value you hold. Migration: replace a
  struct literal with `SttConfig::new("x").with_model_name(Some(..))`, and
  `TtsConfig { options, ..cfg }` with `cfg.with_options(options)`.
- Plugin SDK 0.6: `TurnDetector::evaluate` lost its `audio_context`
  parameter, which the engine always passed as `None`; a detector that
  needs audio has `feed_audio`. Migration: drop the third argument.
- Plugin SDK 0.6: the engine resolves every credential. STT, TTS and text
  processor factories name their variable with `default_api_key_env()`
  (defaulted to none); before `validate` and `build` the engine fills the
  config's `api_key` from it when the section sets none, so no provider
  reads the environment for a key and a provider's tests no longer depend
  on the machine's. `TextProcessorConfig` gained `api_key` and
  `api_key_env` fields (the TOML is unchanged; the OpenAI-compatible LLM
  read both from its options). `secrets::resolve_key` takes the variable
  as an `Option` and an environment lookup and is for hosts;
  `key_available` is gone, and providers use `secrets::configured_key`.
  Google STT: an `access_token` written in the section now wins over a
  `GOOGLE_OAUTH_ACCESS_TOKEN` in the environment.
- Plugin SDK 0.6: errors say whether another attempt can succeed.
  `TextProcessorError` is `#[non_exhaustive]` with a `kind`
  (`TextErrorKind::{Permanent, Transient, RateLimited { retry_after }}`),
  built with `TextProcessorError::new(processor, message)` and
  `caused_by`/`with_kind`; `is_transient()` and `retry_after()` read it.
  `SttError::RateLimited` and `TtsError::RateLimited` carry
  `retry_after: Option<Duration>`, `SttError::from_http_status` maps a
  status the way every hosted recogniser should, and
  `voice_pipes_sdk::reconnect::{backoff_with_jitter, jittered}` spread
  retries. Migration: replace `TextProcessorError { processor, message,
  source }` literals with the constructor, mark a retryable failure
  `with_kind(TextErrorKind::Transient)`, and match `RateLimited { .. }`.
- Every provider now refuses options it does not read: the two Nemotrons,
  Parakeet, Moonshine, Piper, the gRPC clients and the built-in VAD
  backends joined the others (the English Nemotron, Parakeet, Moonshine
  and the VAD backends validated nothing before). Three shipped examples
  carried settings nothing read and lost them: `sample_rate` under
  Parakeet and Moonshine, and `execution_provider = "cuda"` under Piper,
  which ran on the CPU regardless. The gRPC STT/TTS docs described
  `remote_sample_rate` and `remote_provider` options that were never read;
  they are gone from the docs and refused.
- The two LLM crates and the three speech-to-speech providers refuse
  options they do not read and bound the ones they do. The
  OpenAI-compatible LLM refuses `max_retries` past 5, `timeout_secs` of 0
  and `history_max_turns` of 0 instead of clamping them. The engine
  bounds the session settings it reads from `[realtime_provider.options]`
  (`vad_threshold`, `vad_silence_ms`, `vad_prefix_padding_ms`,
  `vad_eagerness`), and a provider's `build` refuses what its `validate`
  would.
- The hosted and local TTS crates, AssemblyAI and Google STT refuse
  options they do not read and bound the ones they do: a zero
  `request_timeout_ms`, a `speed` or voice setting outside the vendor's
  range, or a misspelt key is a validation error, and `build` refuses what
  `validate` would. Pocket's settings no longer show a `pool_size` it
  never read.
- **Breaking for embedders:** `plugins::RegistrationError` is a
  `#[non_exhaustive]` enum (`NameTaken`, `NoSuchProvider`, `TwoDefaults`,
  `NoStage`) instead of a message, so callers can match on what
  went wrong. The six stage registries share one implementation.
- **Breaking for plugins (SDK 0.6):** `ConfigError` is `#[non_exhaustive]`
  and built with `ConfigError::new(field, message)`; it carries a `kind`
  (`ConfigErrorKind::Invalid`, `UnknownName`, `UnknownKey`, set with
  `with_kind`) for code that acts on it, so nothing needs to match on a
  message's words. `unknown_options` and the engine's unknown-provider,
  mode and type errors carry their kinds.
- **Breaking for plugins (SDK 0.6):** `sentence::split_first_sentence`
  takes `at_end`: while text is still arriving (`false`) a mark is a
  boundary only once what follows it has arrived, and at the end (`true`) a
  final mark is one. Chunks keep the whitespace after their mark, so the
  chunks of a text concatenate back to it.
- **Breaking for plugins (SDK 0.6):** `audio::resample`, `resample_f32` and
  `StreamingResampler::new` return `Result<_, ResampleError>`, refusing
  rates outside `SUPPORTED_RATES` (1 kHz to 384 kHz).
  `TtsSessionConfig::default()` is 16 kHz both ways rather than zero.
- **Breaking for plugins (SDK 0.6):** `s2s::codec::encode_pcm16_b64` and
  `decode_pcm16_b64` no longer resample: a protocol encodes and decodes at
  the vendor's rate, and the engine resamples. The `output_emit_rate`
  option is read by the engine.
- The engine's factory registries hand out shared factories:
  `lookup_*_factory` and `*_factories` return `Arc<dyn …>` rather than
  `Box<dyn …>`.
- **Breaking for plugins (SDK 0.5):** every factory builds through one
  method, `build(&self, cfg, ctx: &BuildContext)`, for STT, TTS, text and
  VAD alike. `BuildContext` carries the audit channel, the component's live
  parameters and text chunking, and is `#[non_exhaustive]`, so a new input
  becomes a field rather than another method; `build_with_params` is gone.
  A plugin VAD now receives the live parameter store too, which its adapter
  used to drop.
- A hosted voice's rejected request (4xx other than 408 and 429) is no longer
  retried, by the engine's TTS retry policy or by `TtsError::is_transient`;
  404 now maps to `ConfigError`. A connection that drops mid-body is a
  network error (`voice_pipes_net::BodyError::Read`), not a parse failure.
  `WavStripper::finish` reports a stream that ended inside its header, and a
  WAV `data` chunk ends at its declared size. OpenAI TTS fails a response
  that carried no audio. Vendor parse errors no longer quote the payload
  (`voice_pipes_sdk::secrets::json_error`).
- The CLI's default features are the engine's `batteries` plus the plugin
  crates named in `[package.metadata.voice-pipes] default-plugins` (none
  yet); a workspace rule holds `default` to exactly the two, so a plugin
  becomes a default only by being listed.
- CI's image-features job also builds, lints and tests the engine with
  `component-server`, `stt-grpc`, `tts-grpc`, `post-grpc` and `turn-vap`,
  which no job compiled before.
- **Breaking for plugins (SDK 0.5):** setup-wizard entries left the factory
  traits. A plugin crate exports `wizard_choices()` beside `factory()`, and
  the binary registers them with
  `voice_pipes::plugins::register_wizard_choices(stage, name, choices)`.
  `WizardChoice` gains `menu_rank`: plugin entries follow the built-ins in
  rank-then-label order, no longer in link order. The
  `register_*_factory` functions return a `Result`: a provider name (or STT
  alias) that a built-in or an earlier plugin already has, a second
  `stage_default` in a stage, or entries for an unregistered provider is an
  error instead of a silent shadow, and the CLI exits with it.
- **Breaking for plugins (SDK 0.5):** `TranscriptionResult`,
  `InferenceMetrics` and `TextFragment` are `#[non_exhaustive]`, so a field
  added later no longer breaks a plugin. Build them with
  `TranscriptionResult::new(text, is_final)`, `InferenceMetrics::new()` and
  `TextFragment::new(text)` plus the `with_<field>` setters; struct
  literals, including `..Default::default()`, no longer compile outside
  the SDK.
- **Breaking for TTS plugins (SDK 0.5):** one streaming method,
  `synthesize_stream(req: SynthesisRequest, tx, cancel)`. The request
  carries the text, optional `SynthesisHints` and, for a provider that
  reports word timestamps, the word-timing channel; it is
  `#[non_exhaustive]`, built with `SynthesisRequest::new(text)` and
  `with_hints`/`with_word_timing`. `synthesize_stream_with_hints` and
  `synthesize_stream_with_word_timestamps` are removed. `TtsSessionConfig`
  implements `Default`.
- The engine's text-processor traits and `TextFragment` are the SDK's:
  built-in and plugin LLMs are one kind of thing, with no adapter or
  per-call forwarding task between a plugin and the pipeline. The unused
  `TextFragment::sideband` is removed. `on_user_idle` is no longer a
  processor hook: a session-driving processor registers a
  `voice_pipes::agent::UserIdleHandler` on the session instead. The SDK
  `SessionHandle` gains host data (`with_host_data`, `host_data`), which
  the engine uses to give its own agent gateway the full session handle,
  stored as a type private to the engine so a plugin (even one linking the
  engine) cannot recover it.
  The conversation-memory wrapper now passes warm-up, model and device
  reporting, the session and client tools to the processor it wraps.
- The engine's VAD traits are the SDK's too (`voice_pipes::vad::segmenter::
  VadBackend`/`VadFactory`), and built-in backends read the same resolved
  `VadConfig` a plugin does (`VadSettings::backend_config()`). The engine's
  Silero threshold struct is renamed `SileroThresholds`.
- The engine's speech-to-text and text-to-speech traits are the SDK's:
  `voice_pipes::stt::SttProvider`/`SttFactory` and
  `voice_pipes::tts::TtsProvider`/`TtsFactory` re-export them, so built-in
  and plugin providers are one kind of thing and no adapter sits between a
  plugin and the pipeline.
- One component model. The engine uses the SDK's `Component` family
  (`ComponentCore`, `CoreComponent`, `ComponentKind`, `HealthState`,
  `HealthStatus`, `ComponentTelemetry`, `ErrorPolicy`) instead of a copy kept
  in sync by hand, so a plugin's component needs no adapter. The SDK gains
  `Component::dynamic_schema` and `HealthState::worst`; the never-used
  `Component::turn_descriptor` and `ErrorPolicy::Fallback` are gone.
- The engine stops guessing providers. A pipeline without `[stt]` or
  `[tts]` falls back to Parakeet or Piper only in a build that has them;
  otherwise it says the section is missing instead of naming a provider
  nobody configured. The pipeline graph takes a TTS provider's output rate
  from the provider (`TtsFactory::output_sample_rate`, new in SDK 0.5)
  instead of guessing 24 kHz for Kokoro and 22.05 kHz for everything else,
  which was wrong for OpenAI, Gemini and ElevenLabs; an unknown rate shows
  as unknown. The realtime endpoint observer builds its VAD through the
  VAD factories, so any backend serves it, a plugin's included, and
  validation lists the backends this build has.
- Plugin SDK 0.5 re-exports `async_trait` and `CancellationToken`, which
  every implementation needs, and its crate docs state the pre-1.0 version
  rule instead of a 0.1.x promise.
- One lint policy for every crate: the engine's pedantic and restriction set
  moved to `[workspace.lints]` and every crate opts in (a workspace rule
  checks it). The provider crates moved out of the engine had lost it.
- The local GGUF LLM is a plugin crate, `voice-pipes-llm-gguf`, and a CLI
  default plugin; `text-llm` and `cuda` are CLI crate features, and configs
  are unchanged. The engine no longer depends on candle, and needs
  `tokenizers` only for Moonshine. The conversation-memory wrapper, which
  only the local model used, moved with it, and
  `AUDIT_FULL_PROMPTS_OPTION` is now part of the SDK's text-processor
  contract (the engine re-exports it). The wizard's SmolLM2 entry comes
  from the crate and no longer asks for a tokenizer path: the tokenizer is
  downloaded next to the model, where the processor finds it; the engine's
  wizard has no built-in text processor left and says so in a build
  without one. The pipeline tests that use the local LLM moved to the CLI
  crate (`tests/pipeline_llm.rs`); the streamed case now decodes greedily,
  since a sampled draw from a raw prompt flipped between builds.
- Tracing spans no longer record the text being spoken: Piper, Kokoro,
  OpenAI, ElevenLabs and Pocket-TTS put their `text` argument (and Piper
  and Kokoro their phoneme and token ids) into the synthesis span, where a
  trace exporter or a debug log would carry it. Every `#[instrument]` now
  skips all arguments and names the fields it records (lengths, the voice,
  the speed); a workspace rule holds it.
- Every factory's `build` refuses what its `validate` refuses, as a config
  refusal naming the same settings. Twenty-five did not: Piper, Silero, Kokoro
  and Nemotron 3.5 failed on the missing model before naming a bad option;
  the echo processor and the energy and passthrough VADs ignored an option
  they do not read; the turn detectors (built-in, Smart Turn, VAP) and the
  cloud and gRPC providers refused as a plain failure; and the three
  speech-to-speech providers built without the API key their validation
  required (the bench's synthetic callers now say so at build, not at
  connect). Plugin SDK 0.6: `options::validated` starts a `build`, and
  `testing::build_refuses_what_validate_refuses` checks it; the CLI's tests
  run it on every factory they link.
- The setup wizard's "nothing offered for this stage" message names the
  features the binary registered for it (`plugins::register_build_hint`);
  the engine no longer carries a list of the CLI's plugin features, and
  without a hint the message names a plugin crate for the stage. The CLI's
  list now includes `stt-nemotron35` and `turn-vap`, which the engine's had
  missed, and a test holds every name to a CLI feature.
- Moonshine is a plugin crate, `voice-pipes-stt-moonshine`: `stt-moonshine`
  and `stt-moonshine-cuda` are CLI crate features as before, now linking
  it, and the engine's own `stt-moonshine`, `stt-moonshine-cuda` and
  `ort-cuda` features are gone (the engine opens no ONNX session; it no
  longer depends on `ort` or `tokenizers`). Configs are unchanged and its
  wizard entry keeps its place, second in the recogniser menu. Its sessions
  open through the shared ONNX opener and its session sets come from the
  shared pool, so a saturated pool answers busy after the pool's wait
  instead of blocking without limit, as Parakeet's does. The engine's
  wizard has no built-in recogniser left. `ExecutionTarget::device_report`
  in `voice-pipes-onnx` is the one device report Moonshine and Nemotron 3.5
  share.
- Silero, the VAD `[vad] mode = "auto"` runs, is a plugin crate,
  `voice-pipes-vad-silero`, and a CLI default plugin: `vad-silero` is a CLI
  crate feature and the engine's `batteries` and its own `vad-silero`
  feature no longer contain it. Configs are unchanged: the binary names
  Silero the auto backend (`vad::register_auto_backend`), which `auto` runs
  when the backend can (`VadFactory::can_run`, Silero with its model on
  disk), else energy; the wizard still writes Silero's model under `[vad]`
  and fetches it, from the one wizard entry a VAD plugin may now register.
  The live VAD parameter schema lists the backends the build has.
- Piper, the default voice, is a plugin crate, `voice-pipes-tts-piper`, and
  a CLI default plugin: `tts-piper` is a CLI crate feature and the engine's
  `batteries` no longer contain it. Configs are unchanged, and a pipeline
  with no `[tts]` section still runs Piper, which the binary registers as
  the stage's fallback; its wizard entry keeps its place first in the TTS
  menu. Its session opens through the shared ONNX opener, so `VP_ORT_EP=cuda`
  with `ort-cuda` runs it on the GPU. The schema no longer offers a
  `speaker_id` option that the options reader refused. The engine's Silero
  tests read the recorded speech fixture instead of synthesising it with
  Piper.
- Parakeet, the default recogniser, is a plugin crate,
  `voice-pipes-stt-parakeet`, and a CLI default plugin: `stt-parakeet` is a
  CLI crate feature and the engine's `batteries` no longer contain it.
  Configs are unchanged, and a pipeline with no `[stt]` section still runs
  Parakeet, which the binary registers as the stage's fallback; its wizard
  entry keeps its place first in the STT menu (AssemblyAI, when linked,
  stays preselected). Its sessions open through the shared ONNX opener,
  so `VP_ORT_EP=cuda` with `ort-cuda` now runs it on the GPU (it had stayed
  on the CPU, though the GPU guide said otherwise); CPU stays the default.
  The pipeline integration suite moved to the CLI crate, and Parakeet's own
  suite reads the recorded fixtures instead of synthesising audio with
  Piper. Three items nothing used went: a text-only `detokenize`,
  `vocab_size` and the encoder output's unread `time_steps`; greedy decoding
  uses the shared `argmax` (the first of equal scores wins, and a NaN never
  does).
- The English Nemotron recogniser is a plugin crate,
  `voice-pipes-stt-nemotron`; `stt-nemotron` and `stt-nemotron-cuda` are CLI
  crate features (the images build them as before), and configs are
  unchanged. Its examples moved with it and are renamed (`wer_eval`,
  `offline_transcribe`, `streaming_decode`, `dump_mel_features`, run with
  `-p voice-pipes-stt-nemotron`), and its integration tests read the
  recorded fixtures instead of synthesising audio with Piper. The engine no
  longer depends on `rustfft` at all, and the wizard's built-in tables have
  no manual-export deploy left.
- Smart Turn and VAP are plugin crates, `voice-pipes-turn-smart` and
  `voice-pipes-turn-vap`; `turn-smart` and `turn-vap` are CLI crate features
  (the images build `turn-smart` as before), and configs are unchanged. The
  engine keeps the rule-based `silence`, `provider` and `layered` modes and
  no longer needs `rustfft` for turn detection. The wizard's turn menu lists
  Smart Turn from its crate, which now asks for the model path (same
  default) and declares the model it downloads; the engine's model registry
  dropped its entry. The bench's VAP bake-off (`--features turn-vap`) links
  the crate.
- The three speech-to-speech providers are plugin crates:
  `voice-pipes-s2s-openai`, `voice-pipes-s2s-gemini` and
  `voice-pipes-s2s-elevenlabs`, CLI features `s2s-openai`, `s2s-gemini` and
  `s2s-elevenlabs`, all in the CLI's defaults, so `cargo install` and both
  images carry them. Configs are unchanged. The engine's `realtime` feature
  is now the provider-neutral client and runner alone, without `base64`;
  a build that wants one provider names its feature, which turns
  `realtime` on. The wizard's speech-to-speech menu lists the plugins'
  entries (Gemini Live still first); the engine writes the section header
  and the client-override question. The ElevenLabs agent id is now refused
  when blank, not also when it is the `agent_XXXX` placeholder. The CLI's
  `synthetic-caller` feature links the two providers the callers use.
  Plugin SDK 0.5's wizard fields gain `comment` and `required`.
- The OpenAI-compatible LLM is a plugin crate,
  `voice-pipes-llm-openai-compat`, and the first plugin the CLI builds by
  default (its `default-plugins` list), so `cargo install` and both images
  still carry it. `text-openai-compat` is a CLI crate feature; configs are
  unchanged, function calling and client tools work through the SDK's
  session handle, and the wizard still offers the host server first (its
  LLM default) and the Gemma presets on the `llama-server` sidecar. The
  engine drops the built-in sidecar deploy, which only those presets used.
  Plugin SDK 0.5 adds `WizardField::fixed_literal` for the presets' unquoted
  numbers. The generated providers reference had listed the turn detectors
  under text-to-speech; it now reads one-entry tables correctly.
- OpenAI TTS is a plugin crate, `voice-pipes-tts-openai`; `tts-openai` is a
  CLI crate feature, configs are unchanged, and the setup wizard still lists
  the cloud service and the VibeVoice sidecar right after Piper.
- AssemblyAI is a plugin crate, `voice-pipes-stt-assemblyai`; `stt-assemblyai`
  is a CLI crate feature, configs are unchanged, and the setup wizard still
  preselects it (`stage_default`). The engine drops its WebSocket and URL
  handling for it; the reconnect budget it used was already in the SDK.
- ElevenLabs TTS is a plugin crate, `voice-pipes-tts-elevenlabs`;
  `tts-elevenlabs` is a CLI crate feature, configs are unchanged, and word
  timestamps and the live voice work as before. The first plugin with word
  timing, so karaoke captions now come from outside the engine too.
- Gemini TTS is a plugin crate, `voice-pipes-tts-gemini`; `tts-gemini` is a
  CLI crate feature, configs are unchanged, and the wizard lists it after the
  built-in voices. Its tests no longer change the process environment (which
  needs `unsafe` in Rust 2024 and raced other tests); the two that need no
  `GOOGLE_API_KEY` skip on a machine that has one.
- Pocket-TTS is a plugin crate, `voice-pipes-tts-pocket`, the first hosted
  provider moved out of the engine. `tts-pocket-http` is a feature of the CLI
  crate (`cargo build -p voice-pipes-cli --features tts-pocket-http`); configs
  are unchanged and the voice stays live-editable. The setup wizard lists it
  after the built-in voices. Plugins can now declare a Docker sidecar for the
  wizard to have started (`WizardDeploy::DockerSidecar`).
- **The engine crate has no default features.** `voice-pipes` builds only
  the providers you name; the binary, `voice-pipes-cli`, keeps the old
  out-of-the-box set as its defaults, so `cargo install` and the images are
  unchanged. Migration: builds of `-p voice-pipes` that relied on its
  defaults add `--features batteries`, the same set by name. Three things
  that were always compiled are now features, all in the defaults:
  `wizard` (`config-wizard`), `audio-device` (`live`, `listen`,
  `interactive`, and the ALSA build requirement on Linux) and
  `session-sqlite` (the orchestrator's SQLite store; asking for it in a
  build without the feature is a startup error). `rustfft`, `clap` and the
  WebSocket client are built only by the features that use them, and the
  engine no longer depends on `mimalloc` or enables `tokio`'s test utilities
  outside tests. The engine alone is 162 crates (was 216); a binary with only
  hosted providers (`--no-default-features --features
  stt-assemblyai,text-openai-compat,tts-elevenlabs`) is 179, against 326 for
  the default build.
- Google Cloud STT is a plugin crate, `voice-pipes-stt-google`, and no
  longer part of the engine. `stt-google` is now a feature of the CLI crate:
  build with `cargo build -p voice-pipes-cli --features stt-google` (the
  worker images already do). Configs are unchanged — `provider = "google"`
  selects it as before. `-p voice-pipes --features stt-google` no longer
  exists. The reconnect budget it shares with AssemblyAI moved to the SDK
  (`voice_pipes_sdk::reconnect`), as did the streaming conformance check
  (`voice_pipes_sdk::testing`, behind the SDK's `testing` feature).
- Kokoro TTS is a plugin crate, `voice-pipes-tts-kokoro`, and no longer part
  of the engine. `tts-kokoro` is now a feature of the CLI crate:
  `cargo build -p voice-pipes-cli --features tts-kokoro` (the worker images
  already do). Configs are unchanged. The CLI's `ort-cuda` now also switches
  CUDA on in every linked ONNX plugin, Kokoro and Nemotron 3.5 alike. In the
  setup wizard Kokoro moves to the end of the TTS menu. The engine drops
  `npyz` and `zip`.
- Plugins can offer several setup-wizard entries (`wizard_choices()`, one per
  preset) and mark one as their stage's default (`WizardChoice::stage_default`),
  so a provider that is a stage's out-of-the-box choice keeps being
  preselected after it moves out of the engine.
- Plugins can take live parameters (SDK 0.5). The parameter store moved into
  the SDK (`voice_pipes_sdk::params`, re-exported at its old engine path),
  and the TTS and text-processor factory traits gain a defaulted
  `build_with_params` that receives it, so a plugin voice or LLM can follow a
  voice or prompt changed from the admin console as the built-in ones do. A
  text processor's store is now built from its own schema
  (`AtomicParamStore::from_schema`) instead of a per-type list in the
  engine; the stores it builds are identical.
- The SDK (still 0.4) gains what a plugin voice needs: `sentence` (the
  streaming sentence splitter, re-exported at its old engine path),
  `phonemize` (the espeak-ng runner, likewise), `model_identity` and
  `device_info` on `TtsProvider`, and wizard entries that declare files to
  download (`WizardDeploy::Download`), which the wizard fetches like a
  built-in's.
- `TurnSignal` is a struct rather than an enum. The three variants moved
  to a `decision: TurnDecision` field, joined by `confidence` and
  `expected_end_in`, so a detector that predicts can report how sure it is
  and how much lead time is left instead of only a verdict. `is_complete()`
  is unchanged; construct with `TurnSignal::incomplete()` /
  `likely_complete()` / `forced_complete()` in place of the variants, and
  match on `.decision`. Both new fields are `Option` because a rule-based
  detector has no opinion to report — read their absence as "no opinion",
  never as zero. `build_turn_detector` now returns `Arc` so the runner and
  the STT task can share one detector; `TurnDetectorFactory::build` still
  returns `Box`, so plugins are unaffected. These types are not part of
  `voice-pipes-sdk`.
- **voice-pipes-sdk 0.2.0**: `ParamConstraints` gains `max_len`, enforced
  by `ParamDescriptor::validate` for string values, and
  `ParamDescriptor::mutable_str` now carries a 4096-byte default bound; a
  new `ParamError::TooLong` variant. Plugins that build `ParamConstraints`
  literals must add the field (`max_len: None` keeps the old behaviour).

### Fixed

- The CUDA worker image shipped no ONNX Runtime CUDA execution provider
  libraries, so every ONNX recogniser or voice on it failed to open a
  session under its default `VP_ORT_EP=cuda`. The image build now gathers
  `libonnxruntime_providers_shared.so` and `libonnxruntime_providers_cuda.so`
  from the fetched runtime into `/app`, and fails when they are missing.
- Nemotron 3.5: a question's closing "?" was emitted when the next
  utterance's first chunk ran and opened that utterance's transcript
  ("?  Can you play any song from Pink Floyd"). The provider now drops a
  closing mark carried over from the utterance before; the previous
  utterance, already committed, goes without it.
- Session report: an unanswered wait that spans utterances the recogniser
  found no words in is graded by them. A drop that had words stays severe
  (a lost attempt); a wordless drop grades moderate and names its length
  and the dropped recordings in the capture, which tell noise the caller
  made from speech the recogniser missed. Two sounds before a turn used to
  read as negligible VAD notes beside a severe 17 s recogniser fault on the
  same report.
- Three compose pipelines under `deploy/docker/config/` (Moonshine, the
  Parakeet corpus worker, Nemotron) set `stt.sample_rate`, which the local
  recognisers refuse as unread, so the Moonshine worker rebuilt from the
  tree crash-looped on its own shipped pipeline. The line is gone, and the
  shipped-configs test scans the compose stack's configs too.
- Gemini Live closed a resumed connection with 1008 "session not found or
  expired", a wording the close classifier did not know, so the close
  counted as permanent and the session ended. It reads as a refused
  resumption: the runner starts a fresh session and tells the client its
  context was reset.
- Kokoro ignored `VP_ORT_EP`, which every other local model follows, and
  reported `cpu (fallback)` from its own CUDA probe rather than from what
  the shared opener did. It opens through the shared opener, follows
  `VP_ORT_EP` (its `execution_provider` option still decides when set) and
  reports the device the opener used.
- Piper's `voice` and Parakeet's `model` settings were declared in their
  schemas but never read, and a multi-speaker Piper voice was accepted and
  failed on the first turn, since its graph takes a speaker id the engine
  never passes. The two settings are gone, and a voice whose `.onnx.json`
  states more than one speaker is refused at load naming the file.
- `inspect` showed empty aliases for TTS, text, VAD and turn providers in
  its per-provider details while the listing showed them, and the worker's
  VAD info read the deprecated threshold field; details carry each
  factory's aliases and the VAD info reports the threshold in force.
- `[pipeline] log_format` was parsed and never read: the CLI's `--log-format`
  had a default that always won. The flag applies only when given, the
  config's value is honoured otherwise, and a value other than `compact` or
  `json` is refused at validation.
- The SDK README's quick start and the `vad` module example declared `build`
  returning `anyhow::Error`, which the traits refuse, the README told a
  plugin to depend on a pre-1.0 git tag, and it and two guides called a
  `voice_pipes::run()` that does not exist. The provider example is a
  doctest, so it cannot drift again, and every mention of `run()` names
  what a binary calls.
- Parallel image builds (compose builds the orchestrator and the GPU worker
  at once) failed with `failed to unpack package … .cargo-ok: File exists`:
  both cargo runs unpacked the same crate into one shared cache mount. The
  registry, git and ONNX Runtime cache mounts are locked, so the second
  build waits for the first.
- A TTS provider that reported more word boundaries than the channel holds
  before its first audio frame deadlocked the fragment: the stage drained
  boundaries only after a frame. Word boundaries are taken as they arrive,
  and the relay no longer spins on a closed word channel until the audio
  ends.
- AssemblyAI sent buffered audio as one frame however long (an utterance's
  end flush, audio queued while a session opened), and its streaming API
  ends the session with error 3007 for a frame outside 50 to 1000 ms. Audio
  goes out as the fewest frames of at most one second, split evenly so no
  tail falls under the floor, and an empty end-of-utterance buffer sends no
  frame.
- AssemblyAI and Google STT awaited the vendor socket or a full result
  channel where cancellation was not polled, so a vendor that stopped
  reading, or a pipeline that stopped reading results, parked the task and
  the worker could not be torn down. Cancel is honoured within the
  termination deadline: AssemblyAI polls it beside the audio send and
  bounds its close, and both hand results on through a permit raced
  against cancel.
- A Piper voice whose `.onnx.json` stated a `sample_rate` or `channels` of 0
  loaded and produced frames no sink could interpret; it is refused at load
  naming the file.
- Every TTS probe wrote a WAV into the tap store that nothing removed; the
  probe keeps the newest twenty recordings and deletes the rest.
- A speech-to-speech provider's reconnect back-off (up to 16 s) and connect
  ran inline in the session loop, so for up to 30 s no client frame was
  read, pings went unanswered and a client could give up on a session about
  to recover. They run beside the client: pings are answered, microphone
  audio is dropped until the provider is back, and the session ends if the
  client leaves or the worker stops meanwhile.
- The gRPC STT, TTS and text clients connected and called `GetInfo` with no
  bound, so a black-holed endpoint held worker startup and a service that
  accepted but never answered held it for good; the text client read its
  stream with no idle bound and checked cancel only between messages, so a
  quiet service held the turn until the session ended. Connect and
  `GetInfo` are bounded (10 s), and the text stream races cancel and a 30 s
  idle bound, failing as a transient error.
- A recogniser that stopped taking audio filled the microphone path and
  blocked the session loop, so cancel, barge-in, pings and agent events
  stopped with it. The session hands audio on without waiting, drops and
  counts it while the path is full (one warning per backlog), and a
  recogniser that takes nothing for five seconds is restarted.
- `voice_pipes_net::BoundedLines::finish` kept a trailing carriage return
  on the last line that `push` strips from every other.
- Admin backend: the session detail, the LLM report and the capture-audio
  routes read capture files and concatenated WAVs on the one event loop, so
  a slow disk or a long session held every other request, the liveness
  probe included; that work runs on a thread. Analytics issues are
  persisted as a background task of the request rather than a task nothing
  held a reference to.
- Admin backend: its SQLite stores (the issues feed, the analytics cache)
  sat under `/app/tmp`, owned by the image's uid 10001 while compose runs
  the container as the host user, so neither could be created, the issues
  feed was silently empty and every analytics call re-read every session.
  The stores live in a per-uid directory under the `admin-state` volume
  (`ADMIN_STATE_DIR`, or `ADMIN_STATE_ROOT` plus the running uid); a store
  that cannot open logs once and the dashboard says issues are not being
  recorded instead of "no issues".
- Admin console: `GET /api/v1/pipeline` answered an unreachable orchestrator
  (or an error status, or a body that would not parse) with an invented
  eight-node demo graph and no error field, which the Live Pipeline page
  drew as the running pipeline. It answers `{data: null, error}` like every
  other proxy, and the page says the pipeline is unavailable.
- Admin console: session, experiment and worker ids are percent-encoded
  before they reach the orchestrator's URL, in the backend and the browser
  alike; a `?` or `#` in one became a query or a fragment.
- `docker compose up` with no profile failed on host-port collisions:
  `worker-a` and `worker-b` took the debug ports of `worker-custom` and the
  corpus worker, and `worker-gpu` took the corpus worker's 8803. The A/B
  pair is behind profile `ab` on 8096 and 8097, `worker-gpu` is on 8809,
  and `scripts/compose-check.py` (run by CI) reads the file as Docker
  resolves it and refuses a host port published twice by services that
  start together. The quickstart and the guides describe the services and
  ports as they are.
- The orchestrator rewrote a worker's wildcard listen address with a
  substring replace of `0.0.0.0`, so `10.0.0.0:8081` became
  `1127.0.0.1:8081` and the worker's debug proxy failed; it compares the
  whole host, and maps `[::]` to `[::1]` as well.
- AssemblyAI counted a key term's length in bytes while its message said
  characters; it counts characters, so a 30-letter term in a language with
  multi-byte letters is accepted.
- Every subcommand loaded `voice-pipes.toml` from the working directory
  first, so a file there that did not parse broke `inspect` and stopped
  `validate-config` before it read the file it was given, and the TOML
  error named no file. The config is loaded only by the commands that run
  a pipeline, and a read or parse error names the path.
- A VAD backend or turn detector named by an alias passed the name check
  but skipped its own validation, so a bad model path was found at the
  first session, and the console's turn-detection schema left out an
  aliased detector's settings; both look the factory up by `matches()`.
- Kokoro and Piper returned a success with no audio for text espeak-ng has
  no phonemes for ("...", a lone dash); Piper rendered a tenth of a second
  of noise for it and called that speech. Both fail as the hosted voices do
  (`returned no audio`), a stream skips a silent sentence among spoken ones
  and errors only when none had audio, and Piper's warm-up speaks a word
  instead of ".".
- Silero: a window whose inference failed set the speech probability to 0,
  returned a success and recorded one, so a broken model export looked like
  a healthy detector hearing a quiet room. The chunk returns the error and
  records only the error; an empty probability or a state output of the
  wrong size is an error too.
- A build without the `realtime` feature loaded a `[realtime_provider]`
  section and served sessions that never transcribed or answered; the
  section is refused with a message naming the missing feature, and the
  `[stt]`/`[tts]` exclusivity check runs in every build.
- `[pipeline.audit] enabled = true` with no `captures_dir`, which is what
  the setup wizard writes, recorded into a `captures/` relative to wherever
  the worker ran, with no retention sweep and no audit store, while the
  CLI's own commands used `audit/`: captures were kept forever and not
  listed. The merged config holds one absolute directory whenever recording
  is on (`captures/` under the working directory by default) and every
  reader uses it.
- The HTTP client workers and the orchestrator use for each other could not
  fail to build: a build error was swallowed into a bare client with no
  bearer token and no timeouts, refused by every peer or hung on one, and a
  token no header can carry was skipped silently. The builder returns the
  error, naming the variable and never the value, and the worker fails
  startup with it.
- The sentence splitter panicked on a reply with whitespace wider than a
  byte (a no-break space in "10 EUR", an em space) before a full stop, which
  lost the turn: it sliced one byte after the whitespace.
- `resample_f32` had no rate check, so a gRPC STT request without a
  `sample_rate` (proto3 sends 0) panicked the recogniser's blocking task on
  a capacity overflow. It refuses as `resample` does, and the gRPC STT
  server answers such a request `InvalidArgument`.
- Logs were written to stdout, so `validate-config --format json` printed a
  log line before its JSON and a parser read neither. Logs go to stderr in
  every command.
- A rate-limited LLM call waited as long as the service's `Retry-After`
  said, so `Retry-After: 3600` held the turn for an hour. A turn waits at
  most 5 s for a rate limit; a longer one fails the turn.
- ElevenLabs, Gemini and Pocket TTS reported a success with no audio as a
  successful synthesis, so the engine's TTS retry never ran and the turn
  counted as healthy with nothing spoken. Each returns an error, as OpenAI
  TTS did.
- The OpenAI-compatible LLM's non-streaming call returned the user's own
  words as the reply when the model answered with nothing, so they were
  spoken back. It returns an empty reply, as the local model does.
- OpenAI Realtime with `vad = "none"` sent `turn_detection: {"type":
  "none"}`, a type the API does not have, so the client never drove turns.
  It sends `turn_detection: null`, which turns detection off.
- An English Nemotron export whose `export_metadata.json` gave
  `streaming_config.chunk = 0` loaded, and every transcription then looped
  for ever on the blocking pool. A zero chunk, or a window side past 8000
  frames, is refused at load.
- The GGUF LLM ran an unknown `chat_template` (e.g. `"gemma"` for
  `"gemma3"`) as ChatML and an unknown `device` failed only at build. Both
  are refused when the config is validated.
- The sentence splitter held a sentence ending in a word that ends like an
  abbreviation ("items." like "ms.") until the reply ended, split initials
  apart ("J. R. R. Tolkien" in four chunks) because its initials check never
  saw the next word, and split at a newline mid-stream but not in the whole
  text. Abbreviations are matched as whole tokens, initials keep their name
  ("I." and "A." still end a sentence), and a newline always ends a chunk.
- Pocket TTS's documentation offered a `first_byte_timeout_ms` option the
  provider never read, so a config that copied it was refused as unknown.
  The line is gone: `request_timeout_ms` bounds the wait for the first byte
  as for every later one. A test now holds every option the crate's example
  documents to being accepted.
- A turn whose LLM failed permanently (a refused key, a bad request) was
  retried as if the service were briefly down, and a rate-limited one was
  retried sooner than the service asked. The pipeline retries a text
  processor only when its error is transient, and waits at least what a
  rate limit asked for. A hosted recogniser's refused request (a 4xx other
  than 408 and 429) no longer counts as transient either, so it is not
  reconnected to. The OpenAI-compatible client honours `Retry-After`, and
  every reconnect and retry backoff is jittered, so sessions that failed
  together do not retry together.
- Provider crates no longer panic or wrap on what they parse. ElevenLabs
  TTS truncated `sample_rate` to 32 bits before checking it, so a rate of
  2^32 + 16000 passed as 16 kHz; it is refused. Token counts, rate limits,
  durations and timings from a vendor saturate instead of wrapping; an
  export with fewer outputs, or a tensor shorter than its shape, is an
  inference error; the English Nemotron's and Nemotron 3.5's audio windows
  and caches are read with bounds. Every provider library denies
  `clippy::indexing_slicing`, `unwrap_used`, `expect_used` and
  `cast_possible_truncation` outside its tests.
- A Kokoro voice file with a voice of zero rows or columns loaded (its data
  length matched its shape) and made the first request with that voice
  panic; it is refused at load, and a style lookup reads with a bound.
- `examples/orchestrate/worker.toml` carried an `[output.chunking]` table,
  which belongs in a pipeline config, and the worker schema refuses it: the
  documented `serve --worker-config examples/orchestrate/worker.toml` did
  not start. The shipped-config test had skipped every file that did not
  parse as a pipeline; it now reads each file as its schema and fails on a
  parse error.
- Hosted voices cut long replies off at `request_timeout_ms`, which bounded
  the whole streamed response; they bound the gap between reads now. The
  OpenAI-compatible LLM reports a stalled stream as a stall.
- A streamed sentence split at a `.` before the text after it arrived, so
  "3.14" and "Dr. Smith" were cut; chunks also lost the space after them,
  gluing words in transcripts and histories. Both are fixed in the SDK's
  splitter (see Changed).
- The local GGUF model echoed the prompt when it generated nothing; it
  replies with nothing, and the user's turn is rolled back.
- Speech-to-speech audio was resampled chunk by chunk, with an edge at
  every chunk; each direction is now one stream per session.
- CUDA that failed to register fell back to the CPU silently; it fails.
- An agent that never answered an idle prompt froze the session: no audio,
  no barge-in, no next turn. The offer runs on a task of its own, bounded
  at three seconds, after which the configured idle message is spoken.
- A plugin factory whose constructor consulted its own stage hung the
  process on a registry lock; each factory is constructed once, with no
  lock held.
- An LLM server's mid-stream error ended the turn as a success with a
  partial reply; it fails the turn, and the payload is not logged.
- `threads = 0` meant zero threads in some local models; it means the
  runtime's choice in all of them.
- The resamplers accepted rates they cannot convert (0 Hz, or beyond
  384 kHz); they refuse them.
- A text processor's config error named no entry; `inspect --provider`
  showed only the first stage a name belongs to; the silence timeout had
  three defaults. Each has one answer now.
- OpenAI TTS reported the first model the API listed as the one it used.
- Gemini TTS declared a live voice setting it never adopted, and Pocket a
  live timeout it never read; the voice applies to the next request, and
  the timeout is declared as a restart setting.
- Credential headers outside `bearer_auth` (AssemblyAI, Google) were not
  marked sensitive.
- Local-model prompts did not match each model's template: Llama 3 got two
  begin-of-text tokens and Gemma 3 a system role it does not have.
- The GGUF `prefill_cache` never took effect: the whole prompt was fed
  from position zero, replacing the cache. A prompt starting with the
  prefilled tokens is fed from where they end, on a backend that can take a
  batch at an offset. Neither candle architecture can (Llama's mask fails to
  broadcast, Gemma 3's mixes dtypes), so the option is refused at load with
  a warning for every model, and it defaults to false, as it always did
  despite the docs. The history is
  capped in tokens, within the model's context, not in bytes.
- OpenAI Realtime truncated function-call items on barge-in and sent
  semantic VAD as server VAD. Tool calls without an id or a name are one
  recoverable error from every S2S provider, and blank arguments are `{}`.
- A turn dropped before its final kept its audio in the audit buffer, so
  the next turn's recording began with it.
- The English Nemotron's pooled sessions could leave the pool apart from
  their permit, and a failed engine build then made the next checkout
  panic; a pooled item and its permit now travel together.
- Smart Turn and VAP held their model's lock while audio was fed, so
  feeding waited for inference; VAP's cadence was stamped for skipped runs.
- The English Nemotron read a padded final chunk with the wrong row
  stride, and its stream fed the encoder features that differed from an
  offline pass at every window edge; it now computes the same features.
- AssemblyAI in its default mode ended the whole stream with the first
  utterance, so the pipeline restarted it per turn with failure backoff;
  the stream now lasts the session, one vendor session per utterance.
- Gemini Live's output transcript lost words: pieces were diffed as if
  cumulative, so " the", " then" became " the" + "n". They are appended
  as sent.
- The English Nemotron sized its state tensors by multiplying shape
  dimensions from the export's metadata into an `i64` and casting: a
  negative dimension or an overflow wrapped into a wrong allocation. It is
  a checked count now, and a broken shape is refused with the state's name.
- Gemini Live ignored `[realtime_provider.options] output_emit_rate`,
  which the OpenAI and ElevenLabs providers honour; it validates and
  applies it now.
- Parakeet, Moonshine and the English Nemotron glued two utterances into
  one when the second was already queued as the first finished (a slow
  decode, or a quick reply): their "take what is queued" loop read past
  the utterance boundary. On the test fixture Parakeet returned one final,
  "Hello how are you? Hello how are you?", and never finalised the second.
  Each stops at `is_last` now. Nemotron 3.5, which already stopped there,
  also took chunks arriving while it drained, so an unpaced sender could
  hand it a whole utterance in one run and no interim came out; its
  streaming test failed every time it ran alone. Every drain now takes
  only what was queued when it started. The SDK's `testing` module gains
  `back_to_back_utterances_stay_apart`, which queues both utterances
  before the stream reads anything.
- The local GGUF and OpenAI-compatible text processors refuse an option
  they do not read (`TextProcessorConfig::unknown_options`, new in SDK
  0.5), as the turn detectors do. It found four examples setting
  `endpoint` on the OpenAI-compatible processor, which reads `base_url`
  and had ignored it since May: `live-gemini-tts.toml` meant a local
  server on port 1234 and was calling OpenAI. They now set `base_url`.
  The local-GGUF guide and the first-agent tutorial showed a `tokenizer`
  key the processor never read; the tokenizer is found beside the model,
  and `tokenizer_path` points elsewhere when it is not.
- The setup wizard works out every stage's menu before the first
  question. A build that cannot fill either mode stops at once, naming
  each empty stage and the features that would fill it, instead of after
  the STT questions; a build that can fill only one mode is not offered
  the other.
- An utterance thrown away as too short by the silence timer was
  recorded twice in the session capture, so a grader counted two drops
  for one. It is recorded once, as on the path that decides at the
  recognition result.
- Smart Turn and VAP report a fraction where a whole number is expected
  as a type error rather than as a range the value is inside;
  `smart_min_audio_ms` and `vap_min_interval_ms` are bounded at 10000,
  the bound the console shows; and the console shows their settings as
  restart-only, since nothing changes them live. The Smart Turn wizard
  entry names the model it downloads (v3.2), not v2. Its log-mel front end
  builds its window, filterbank and FFT plan once instead of per pause.
- `examples/test-live-assemblyai.toml` named the turn mode
  `provider-signal`, which never validated; it is `provider`. A CLI test
  now validates every shipped config with the plugins linked.
- Four messages (the turn-mode description in the console, the worker's
  session-capacity warning, the speech-to-speech reconnect notice and the
  GGUF tokenizer error) carried a run of spaces where a line continuation
  had been lost.
- Parakeet no longer drops an utterance whose final decode comes back
  empty after an in-progress decode of the same audio found every word.
  Greedy TDT decoding can lose a whole utterance on one near-tie at the
  first word (the prediction network then never leaves its start state),
  and how much quiet trails the speech is enough to tip it; the caller
  heard no reply and had to repeat themselves. The final now falls back to
  the earlier decode's words (or re-decodes the prefix it came from when
  that decode saw only the trailing window), logs a warning, and counts it
  in the component's `empty_final_rescues` extra.
- Google STT panicked on a response carrying a negative time offset of
  `i64::MIN` seconds (the protobuf duration conversion overflows); such an
  offset is now dropped.
- The engine builds and passes its tests with `--no-default-features`, and CI
  keeps it that way: one provider-specific wizard parameter and two test
  modules assumed the default providers were compiled in.
- Session metadata in the diagnostic report and session view. The STT mode
  was guessed from a hardcoded provider list that lacked Google, so Google
  STT sessions were reported as batch and flagged `stt.processing_high` for
  time that was never on the user's wait; the engine now records `stt_mode`
  in `metadata.json` from the provider's declared `Streaming` capability and
  the report reads it first. The TTS voice was always null because it was
  read only from trace events the shared TTS provider never writes; it is now
  recorded as `tts_voice` in `metadata.json`. The pipeline config hash and
  sample rates were always null on the orchestrator's session record (the
  worker never set them) and that null overwrote the capture's value in the
  admin backend; the worker now sends them on the session start record, the
  orchestrator keeps them on the end record, and the backend no longer lets
  an orchestrator null hide a capture value. The report's event timeline
  listed each client timing event a second time as a row with no time or
  payload (its copy from `pipeline_events.jsonl`); those copies are dropped
  when the trace copy exists, other pipeline rows get their fields as the
  payload, and client timing rows are ordered by their client timestamp in
  the session event log.
- Conversation memory bounds. The rolling history window is capped in
  bytes (256 KB) as well as turns, an agent context entry whose
  generation fails is evicted like any other (a thousand failing
  context turns used to grow the window without limit), and the
  OpenAI-compatible processor keeps the heard prefix of a reply in
  history when the stream fails mid-turn, as the local-model wrapper
  already did. The history lock no longer poisons.
- Redaction unified. The config redactor, the provider frame scrubber
  and the SDK's parameter passthrough each had their own idea of what a
  secret key is; they now share one test (`api_key`, `apikey`,
  `secret`, `password`, `passwd`, `authorization`, `bearer`,
  `credential`, `private_key`, `token` / `*_token`, exact `key`,
  case-insensitive with `-` and `_` alike) and one walker. Any value
  under a secret key is blanked whatever its type and depth (a numeric
  token or a nested `client_secret` object used to pass), and
  URL-shaped strings lose their userinfo and secret-named query
  parameters. Passthrough option values are scrubbed the same way.
- Capture budgets and durability. The mic audio held for the turn in
  progress on the cascade path grew without bound until a final result
  arrived; it is capped at 120 s of the newest audio. The retention
  sweep now repeats every six hours instead of running once per process,
  treats directories holding an events log or a mic recording as
  sessions (a session whose `metadata.json` write failed was never
  reaped), never follows symbolic links, and `metadata.json` is written
  before any other artefact. New `[pipeline.audit] max_session_bytes`
  (default 512 MiB) stops audio artefacts past the budget with one
  warning while text artefacts continue. Capture files are written to a
  temporary sibling and renamed into place; audit writers disable
  themselves after ten consecutive store failures instead of logging
  every event; the file-backed session store deletes the files of
  evicted records and trims oversized directories at load; capture blobs
  are streamed to the admin API instead of read whole. The `llm_prompt`
  trace event carries the message count and a hash unless
  `[pipeline.audit] full_prompts = true`. Client tags are capped at 16
  entries of 64 bytes at the handshake. The admin backend answers 404
  instead of 500 for a truncated capture WAV.
- Tool identity and agent frame bounds. A client could declare a tool in
  `configure` `options.client_tools` with the same name as a server-side
  tool and have the model's call routed to the browser instead of the
  operator's agent; runtime-injected tools whose name belongs to a server
  tool are now dropped at session setup, logged, and reported to the
  client with a new `warning` error code 1007 (`ClientToolRefused`).
  Frames from a gRPC agent are bounded: 1 MiB per message at the
  transport, 64 KB per text field, `Say` (and cascade `CompleteTool`)
  throttled to 4 per 10 s, agent events waiting for an idle session
  capped at 16 with the oldest dropped, `CompleteTool` accepted only for
  a call acked as Pending (once, within 5 minutes) so a late completion
  cannot contradict a reported timeout, and agent `Message` frames with
  the engine's own `trigger_action` / `client_tool_result` kinds
  refused.
- The OpenAI-compatible text processor bounds what the upstream sends
  and how long it may take. Input containing chat-template markers
  (`<|im_start|>`, `<|begin_of_text|>`) was sent as a single user message
  with no system prompt or history, letting transcribed speech replace
  the operator's prompt; it is now always plain content on top of the
  system prompt and history. Streamed `tool_calls[].index` is capped at
  64 (it sized an allocation), tool-call arguments at 64 KB, response
  text at 256 KB, one SSE line at 1 MiB and a non-streaming body at
  1 MiB. A stream ends at `finish_reason` or `[DONE]`, so a server that
  never sends `[DONE]` cannot hold a turn open. `timeout_secs` is now the
  longest wait for the next byte (headers, then each chunk) instead of a
  whole-request budget that cut long answers; the TCP connect has a 5 s
  budget of its own. Retries are clamped to 5 with the back-off capped at
  8 s, and a barge-in interrupts a back-off wait at once. Upstream error
  bodies are logged on the worker (truncated) and no longer forwarded to
  the client, which sees the status code and its class; the config's
  `Debug` form redacts the API key.
- `voice-pipes serve` (and `text`) now validate the merged config before
  starting, like every other command; a worker no longer comes up on a
  config that fails the checks. An unknown or disabled `turn_detection.mode`
  is an error instead of a silent fallback to the silence detector, at
  validation and at session setup. `pool_size = 0` is rejected (it built an
  empty engine pool whose first checkout waited forever, and Piper and
  Kokoro now clamp it as well). Unknown keys under the worker file's
  `[orchestrator]`, `[pipeline.text_chunking]` and the orchestrator config
  are errors. Validation covers the ranges the runtime relies on
  (transport port and capacity, write timeout, audit buffer, idle timeout,
  adaptive-quality tier ordering, circuit breaker, endpoint observer, VAD
  thresholds) and caps repeated tables and prompt lengths; narrowing
  option casts are checked. `knowledge_base_path` resolves against the
  config file's directory as documented. Auto-discovered configs are
  logged with a warning and their absolute path.
- Realtime provider clients bound what the provider sends: WebSocket
  messages are capped at 4 MiB (tungstenite's 64 MiB default was the only
  limit), an audio field at 1 MiB of base64 and tool-call arguments at
  64 KB, and a provider-announced sample rate outside the supported set is
  refused (ElevenLabs could announce `pcm_1` and have the resampler ask
  for gigabytes). A transport error or a non-JSON frame no longer ends
  the session as a fatal error: the error becomes a close the runner can
  reconnect from, the bad frame is skipped. The worker pings the provider
  every 20 s and reports a connection with no frame for 60 s as closed,
  outbound sends are bounded so a peer that stops reading cannot hold the
  socket past the session, reconnects back off exponentially, and a
  session gives up after ten reconnects. Client-supplied Gemini VAD
  timings are clamped; the connection target's `Debug` no longer prints
  the key-bearing URL or header values.
- Config wizard: every typed value is rendered through the TOML serializer
  (a Windows path or a quote used to produce a file that failed at worker
  start) and both generated files are parsed back before anything is
  written; output chunking moves to the pipeline `[output]` table where the
  engine reads it (the worker file's copy was ignored; `WorkerConfig` now
  rejects unknown tables so this cannot recur, and the shipped worker
  configs were updated); the three cloud-TTS key prompts are hidden like
  the others; keys are trimmed and refused if they contain spaces or
  quotes; `.env` is written owner-only via a temp file and rename, and
  before the config files; an existing generated config is not replaced
  without a yes; the ElevenLabs agent-id placeholder is refused; the
  wizard refuses to run outside `voice-pipes/`; the shell launcher strips
  quotes around `.env` values like the PowerShell one. `client_overrides`
  is explicit for every realtime profile and includes `instructions` only
  after an explicit yes.
- Fleet: every HTTP request between orchestrator and worker is bounded
  (3 s connect, 10 s request; the params proxy's bound is `[workers]
  proxy_timeout_s`), and a worker forwards turn records on their own
  tasks so a stalled orchestrator no longer stops the worker's local turn
  history. Dead workers are removed from the registry after `[workers]
  dead_grace_s` (default 600) and only live workers count against
  `max_registered`, so a fleet of restarting workers can no longer fill
  the registry and be refused. A live worker's id cannot be re-registered
  from a different host (409). A turn event no longer decrements the
  worker's active-session count.
- Admin API: `GET /api/v1/metrics` and `GET /api/v1/turns` ask the store
  for the window or page they need instead of cloning every turn record
  per request (SQL pushdown on sqlite, a locked scan on the in-memory and
  file stores); a rolling-window string ending in a multi-byte character
  no longer panics the connection; an absurd `page` yields an empty page
  instead of overflowing; a query string with a flag-style pair before
  `token=` is accepted; CORS is applied outside the auth layer so browser
  preflights are answered, and it now allows `Authorization`, `PATCH`,
  `PUT` and `DELETE`; a malformed blob key is a 400, a proxy failure
  reports its kind without the worker URL; the open transport `/health`
  no longer includes per-component free-text reasons (they stay on the
  authenticated `/debug/components`).
- Adaptive quality: downgrade streaks are kept per signal, so the
  once-per-frame socket-write sample can no longer reset the streak of the
  once-per-turn playback report (a link with steady underruns never left
  `full`). The send-latency sample is reported once per second, the ping
  RTT bucket and smoothed RTT now feed the controller, and sparse
  observations merge into the published link view instead of blanking the
  other fields, so `quality_update.quality` reflects the real ping bucket.
  The unused `poor_rtt_ms` health threshold is gone: `lost` is decided by
  missed pongs only.
- The audio pacer keeps an absolute release schedule. While TTS streamed a
  burst of chunks nothing was released until the source went quiet, and the
  backlog then drained at about twice the configured pace; releases now
  happen on schedule under steady input and a backlog drains at exactly
  `pace_factor` times real time. Stereo chunk durations no longer count each
  channel as its own frame.
- Interrupts arriving after a turn already completed on the server (the
  user talks over the tail the client is still playing) now cut only that
  tail: one `clear_audio`, no second `turn_metrics`, and the turn is counted
  once in the session summary; the handler no longer waits out the metrics
  grace period on an empty channel. A `cancel` with no turn in flight
  produces only a `clear_audio` and leaves the state machine in Listening.
  Typed text mid-turn goes through the same interrupt path as `cancel`
  (which also restores the stateful-TTS cancel it had skipped). A
  `playback_complete` while the pipeline is still producing audio is
  ignored, and a metrics snapshot left over from an abandoned turn is
  discarded before the next turn starts.
- Every classic-runner state change goes through the turn-state machine's
  verdict: the client receives `state_changed` only for states the server
  entered, and rejected transitions are logged. A new utterance committed
  while the previous turn's tail is still playing (Draining) now starts the
  next turn instead of being refused while the client was told
  `processing`.
- Realtime: outbound-audio suppression is armed only together with a
  `clear_audio` and released by the matching `playback_complete` or the
  next response, so a provider `response.cancelled` arriving after the
  client already acknowledged the flush can no longer mute the session.
  Response terminal events are matched by response id; the cancelled
  response's terminal event after an agent `Interrupt` no longer re-opens
  the gate for a second `response.create`. A client cancel persists the
  interrupted turn's own user transcript, and a transcript-proxy TTFB
  anchor no longer replaces a provider speech-stopped anchor.
- The STT task accumulates every final result until the turn detector
  completes (a pause inside one utterance no longer drops the earlier
  segment); providers whose finals re-send the growing segment declare
  the new `SttCapability::CumulativeFinals` (AssemblyAI does) and keep
  superseding semantics per segment. The end-of-utterance sentinel carries
  the response text as soon as the LLM's final fragment arrives instead of
  waiting up to 500 ms for the stream to close.
- Hosted STT reconnects are budgeted. AssemblyAI and Google reset the
  attempt counter only after a session that lived 10 s or delivered a
  transcript, cap reconnects at 20 per five minutes, and end the worker
  with the new `SttError::ReconnectExhausted` instead of looping when a
  provider accepts every connection and then rejects the stream. The
  backoff multiplier no longer overflows to a zero delay.
- Google STT: outbound requests are queued locally and handed to the RPC
  one permit at a time, so a server that accepts the call but neither reads
  nor answers can no longer wedge the worker (cancel is honoured while 35 s
  of audio is backed up); a backlog the server is not draining sheds its
  oldest audio instead of growing without bound; an utterance that ends while the previous stream
  is draining is half-closed on its own stream instead of waiting for
  audio that never comes; credential fetches are bounded (30 s) and
  cancellable.
- AssemblyAI: the WebSocket connect is bounded (10 s) and abandoned on
  session teardown; a socket that breaks while waiting for `Termination`
  ends the session instead of reconnecting; the connect span carries the
  attempt number and session id.
- Parakeet TDT: the predicted duration now advances the decoder after a
  non-blank token too (it was applied only to blanks), so the greedy walk
  no longer re-reads the same frame and emits repeated or garbled tokens
  on fast speech. Nemotron loads a pool of session sets (`pool_size`,
  default 2) and a second concurrent session waits up to 5 s for a free set
  instead of failing "already in use" (after that it fails with an error
  that names `pool_size`); encoder cache tensors are moved into each run
  instead of copied.
- Nemotron, Parakeet and Moonshine keep one `transcribe_stream` call alive
  across utterances and number them with `segment_index` (Nemotron used to
  return after the first `is_last`, forcing a worker respawn per turn).
- espeak-ng receives the text on stdin: a sentence starting with a dash (a
  markdown bullet in a Kokoro or Piper reply) was parsed as an option and
  the fragment was dropped; long paragraphs no longer hit the argv limit.
- The audio resampler now low-passes at the output Nyquist when
  downsampling. Every 48 kHz mic to 16 kHz STT path (and 24 kHz TTS to
  16 kHz output) previously folded 8 to 24 kHz content into the speech band;
  a 10 kHz tone is now attenuated by more than 40 dB across 48 to 16 kHz with
  the pass-band flat within 0.5 dB. The streaming resampler keeps enough
  history for the stretched kernel (and for 1-sample chunks). Silero VAD
  uses a stateful resampler across chunks instead of a fresh kernel per
  20 ms.
- The VAD filter re-derives its framing and silence budget when the input
  sample rate changes mid-session; `turn_detection.silence_timeout_ms = 0`
  is rejected at config time; the per-turn `tts.chars` metric counts the
  agent text actually synthesised (in characters) instead of the user
  transcript (in bytes).
- A client that stops reading its WebSocket no longer pins its session:
  writes are bounded by the new `[transport] write_timeout_ms` (default 5 s),
  heartbeat pings never block on a full outbound queue, and the read task is
  stopped on close.
- A streaming STT worker that ignores cancellation is aborted when it is
  respawned or when the session ends, instead of leaking a live provider
  stream per occurrence; a closed mic channel now bounds how long the STT
  task waits for the worker's last results.
- Accepted speculative turns now complete: the forward loop sees end-of-
  stream, `TurnMetrics` is emitted, and no task or ledger leaks per turn.
  The speculative coordinator no longer holds its state lock across a
  bounded send.
- Realtime provider connects are bounded (10 s) and a reconnect resets the
  stale response/agent bookkeeping; the realtime runner now drains client
  frames during provider/agent connect like the classic runner does, so an
  early-streaming client cannot trip the heartbeat before the loop starts.
- The component server's unary TTS `Synthesize` no longer deadlocks on
  providers with native streaming; TTS and text streaming RPCs cancel the
  provider when the client disconnects; remote param updates time out after
  5 s.
- Zero-channel frames (proto3 default) and zero sample rates from providers
  are handled instead of panicking; ONNX output shapes are checked before
  indexing (Parakeet, Moonshine); AssemblyAI validates `sample_rate`.
- Server-initiated session endings flush `SessionSummary` / `Closing` before
  the pump is cancelled; realtime sessions now send `Closing` too.
- `tts_task` no longer spins when the fragment channel closes mid-fragment;
  a barge-in no longer aborts the task delivering `TtsSession::cancel()`;
  the mic tee into a stateful TTS session uses `try_lock` so it cannot park
  the session loop.

### Security

- Nothing publishes from a commit whose tests did not pass:
  `scripts/require-green-ci.sh` waits for `core-tests` on the tagged commit
  and fails unless a run of it passed, and the crates, images and release
  workflows run it before anything is published. Each container image is
  built into the local daemon, its non-root uid checked, and only then
  pushed and signed; the check used to run after the push. The release
  guide names the tag-protection ruleset to set by hand in the repository
  settings.
- The admin frontend sends `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY`, a `frame-ancestors 'none'` content security
  policy and a `Referrer-Policy` on every response, and `index.html` (and
  the routes that fall back to it) is `Cache-Control: no-cache`, so a
  browser cannot keep a previous release's entry document while its bundles
  are immutable.
- The Playground no longer holds the worker's client token.
  `/api/v1/auth/check` handed `VP_CLIENT_TOKEN` to every signed-in browser,
  which kept it in `localStorage` and sent it to the worker as `?token=` in
  the WebSocket URL, where access logs and any script on the page could
  read it. The console connects through the backend's
  `/api/v1/playground/ws?worker_id=` proxy, which admits only a worker the
  orchestrator lists (4403 before the handshake otherwise), connects to the
  address the worker registered (loopback and wildcard hosts mapped to
  `PLAYGROUND_LOOPBACK_HOST` in a container), presents the token upstream
  as a bearer header, closes the browser with a code and reason when the
  orchestrator or the worker cannot be reached or a frame is oversized, and
  bounds frames and queues. `/api/v1/dispatch` answers the proxy's
  `ws_path` instead of the worker's `ws_url`; `/auth/check` carries no
  client token and the console deletes one an older build stored;
  `withClientToken` and `VITE_VP_CLIENT_TOKEN` are gone, and
  `PLAYGROUND_LOOPBACK_HOST` replaces `TRANSPORT_HOST`/`TRANSPORT_PORT`.
  A Direct URL session still opens the URL it is given, with no credential.
- The admin backend accepts `?token=` only on the WebSocket, SSE and audio
  routes that cannot send a header; anywhere else it is ignored and the
  request refused, so a `curl` user following the API page no longer writes
  the admin token into the access log. The API reference says where the
  query parameter is accepted.
- Admin backend request bodies are bounded: 2 MiB on the probe routes (the
  worker itself takes 960 000 bytes of audio), 1 MiB elsewhere, refused with
  413 `PAYLOAD_TOO_LARGE` before anything is read, and a chunked body is
  stopped at the first chunk that crosses the ceiling; the benchmark bodies
  are typed models. A probe WAV was held whole for up to 180 s with no cap.
- Admin console: a failed orchestrator call was answered with the `httpx`
  error's text, which names the internal URL or the host and port a connect
  was refused on, on every proxy route and the fleet SSE stream. Every proxy
  answers a fixed code (`ORCHESTRATOR_ERROR` with the HTTP status, or
  `ORCHESTRATOR_UNREACHABLE`) and a message made from those alone; the
  exception goes to the server log.
- The llama-server, pocket-tts and VibeVoice sidecars, which have no
  authentication of their own, publish on `127.0.0.1` only, and llama-server
  and pocket-tts run as the host user's ids rather than root, as the
  deployment guide already claimed; pocket-tts keeps its caches in
  `models/.cache/pocket-tts`, which the launcher creates.
  `scripts/compose-check.py` refuses a sidecar or a worker debug API
  published beyond loopback.
- ElevenLabs Agents with `public_agent = true` still attached the resolved
  API key, and since validation assumed a public agent sends none a plain
  `ws://` endpoint passed, so the key went over an unencrypted socket. A
  public agent sends no key, even when one is set; the guide says so.
- Log lines carry no conversation text at any level. Transcripts, prompts,
  fragments, replies and a vendor's error body were logged at info and
  debug in the engine and in the Moonshine, Piper, GGUF and
  OpenAI-compatible plugins; they log lengths and a kind now, an upstream
  error body is reported as its status, kind and length, and a workspace
  rule refuses an event macro field or format argument named for a payload
  (`text`, `prompt`, `fragment`, `output`, `transcript`, `reply`, `input`,
  `body`).
- The launchers print where the admin sign-in token is written, not the
  token, as `SECURITY.md` promised.
- The release workflow's GitHub Actions are pinned to commit SHAs.
- Providers cannot loosen the network policy. `voice_pipes_net::http_client`
  returns a finished client (the TLS stack, timeouts, no redirects, warm
  pooled connections); `client_builder` is gone, and the net crate
  re-exports `reqwest` and `tokio_tungstenite` so provider crates no longer
  depend on them. A workspace `clippy.toml` refuses building clients any
  other way, and the workspace rules refuse a direct HTTP or WebSocket client
  dependency in a provider crate.
- Google STT refuses to send its OAuth token over plain HTTP to a host on
  the internet, as every other hosted provider already did; a local test
  server and the `allow_insecure_endpoint` opt-in still pass.
- Hosted providers read the network to a bound: error bodies (8 KiB), the
  OpenAI TTS model listing, Gemini TTS responses (32 MiB) and ElevenLabs'
  NDJSON lines (4 MiB) through new `voice-pipes-net` helpers
  (`error_body`, `limited_body`, `BoundedLines`). AssemblyAI logs an
  unparseable frame's length, never its text, which can hold a transcript.
  Every hosted crate now tests malformed input.
- Serialising a provider config (the config hash, a display) writes
  secret-named options as `***REDACTED***`, as `Debug` does; the pipeline
  config hash no longer depends on an LLM key, so rotating one leaves it
  alone. The wizard's rendered choice no longer prints typed keys in
  `Debug`.
- `rustls` 0.23.40 → 0.23.45 for RUSTSEC-2026-0285 (TLS 1.3 handshake
  messages accepted across encryption levels). `cargo deny` now checks every
  feature of every workspace member rather than one feature set of the
  engine, so image-only and plugin dependencies are covered too.
- Network clients follow one policy, from the new `voice-pipes-net` crate:
  `rustls` on `ring`, installed once at startup; a connect timeout and an
  overall or per-read bound on every request; bounded WebSocket messages
  (AssemblyAI's were unbounded up to tungstenite's 64 MiB default, now 1
  MiB); and **no redirects** for provider requests. `reqwest` drops
  `Authorization` on a cross-host redirect but not custom key headers, so an
  endpoint that redirected could pass ElevenLabs' `xi-api-key` or Gemini's
  `x-goog-api-key` to another host. A provider now also refuses to start if
  a changed `endpoint`/`base_url` would send its key in plain text to an
  internet host; local hosts pass, and `allow_insecure_endpoint = true`
  opts in (see the providers guide). **Possibly breaking**: such a config
  now fails validation. None of the shipped configs does.
- Text processors are validated through their factories at config load, as
  every other stage is. An unknown `type` fails validation instead of the
  first build, and the OpenAI-compatible processor's own checks (sampling
  ranges, `tool_choice`, `base_url`) take effect; before, they never ran.
- Every dependency must be audited or explicitly exempted (`cargo vet`, a new
  CI job). The audits Mozilla, Google, the Bytecode Alliance, ISRG, Zcash and
  Embark publish cover 114 of today's crates; the rest are the recorded
  baseline, and a crate added from now on needs a review or a stated reason.
- Release binaries are built with `cargo auditable`, ship a CycloneDX SBOM
  and carry a GitHub build-provenance attestation; container images carry
  BuildKit provenance and an SBOM, a provenance attestation in the registry,
  and a keyless `cosign` signature. `SECURITY.md` says how to verify them.
- A second version of any crate now fails CI (`cargo deny`,
  `multiple-versions = "deny"`). The 49 older versions third-party crates
  bring in today are listed by exact version with who pulls them, so the same
  crate at a new version still fails. `toml` moved to 1.x, which removes its
  own three duplicates.
- Credentials cannot be printed by accident. Plugin SDK 0.5 adds
  `voice_pipes_sdk::secrets::Secret`: `Debug` and serialisation write
  `***REDACTED***`, it has no `Display`, and it is wiped from memory when
  dropped. `SttConfig::api_key` and `TtsConfig::api_key` are now
  `Option<Secret>`, every hosted provider keeps its key as one and exposes
  it only where it is sent, and the configs' `Debug` blanks secret-named
  options: the OpenAI-compatible processor's key, which lives in its options,
  used to print in full with `{:?}` of a pipeline config. **Breaking for
  plugins**: read a key with `.expose()` and build one with `Secret::from`.
  The config hash now reflects whether a key is set rather than its value,
  so rotating a key no longer changes it.
- One TLS implementation: `rustls` on the `ring` provider, everywhere.
  `reqwest` moved from 0.11 (OpenSSL through `native-tls`) to 0.12 on
  `rustls`, the workspace no longer enables `rustls`' default `aws-lc-rs`
  provider, and ONNX Runtime's build-time download uses `rustls` too.
  `cargo deny` bans `native-tls`, `openssl` and `aws-lc` from the graph. This
  retires the `h2` 0.3 advisory exception. `tokio-tungstenite` 0.24 → 0.29,
  the version `axum` already used. Builds lose 13 to 20 crates each.
- Provider crates must `forbid(unsafe_code)`, may depend only on the SDK and
  the shared infrastructure crates, and each has a CODEOWNERS entry; CI
  enforces all three.
- Admin console backend now requires its own bearer token (`ADMIN_UI_TOKEN`)
  on every HTTP, SSE and WebSocket route; the console shows a sign-in form
  and the launcher generates the token into `voice-pipes-admin/.env`. Set
  `ADMIN_UI_AUTH_REQUIRED=false` to keep an open loopback dev setup. The
  admin compose file now publishes ports 8000 and 3000 on `127.0.0.1` only.
- Shipped deploy configs and wizard output now set `[auth] required = true`;
  the launcher generates `VP_CLIENT_TOKEN`, `VP_ADMIN_TOKEN` and
  `VP_INTERNAL_TOKEN` into `voice-pipes/.env` and compose passes them
  through. Worker debug ports are published on loopback only. The console
  receives the client token after sign-in so the Playground keeps working.
  **Migration:** existing `voice-pipes/.env` files gain the three tokens on
  the next launcher run; stacks started with `docker compose` directly must
  set them (or set `required = false` for a loopback dev box).
- Session capture is now an explicit choice: the wizard asks, the shipped S2S
  and `openai-host` profiles no longer record, and a new
  `[pipeline.audit] retention_days` (default 7) deletes older session
  directories at worker start. Set `retention_days = 0` to keep the previous
  keep-forever behaviour.
- All container images now run as an unprivileged user (uid 10001; nginx
  101). Compose sets `user: "${VP_UID:-10001}:${VP_GID:-10001}"` on services
  with bind mounts and the launcher exports the host user's ids on Linux.
  **Migration:** on Linux, if you run `docker compose` directly, export
  `VP_UID`/`VP_GID` or `chown -R 10001:10001` the `models/`, `captures/`,
  `audit/`, `sessions/` and `benchmarks/` directories.
- Realtime sessions no longer let a connecting client replace the operator's
  system prompt or set arbitrary provider options. `[realtime_provider]
  client_overrides` (default: the VAD tuning knobs) lists what a client may
  change; refused keys are reported in `session_ready.negotiated.ignored_options`
  and client `instructions`, when allowed, are appended after the operator's.
  **Migration:** add `"instructions"` to `client_overrides` where the
  Playground's system-prompt field should keep working (the wizard does this
  for the dev profile).

[Unreleased]: https://github.com/myned-ai/voice-pipes/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/myned-ai/voice-pipes/releases/tag/v0.1.0

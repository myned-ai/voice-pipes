# Troubleshooting

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

Symptom first, then the likely cause and where to look. The turn-state
runbook for gateway-level debugging is in
[Gateway: transports and engines](../extending/gateway-transports-and-engines.md#5-operator-runbook-state-machine-aware-debugging).

## Startup and configuration

| Symptom | Cause | Fix |
|---|---|---|
| `unknown or disabled provider` at worker start | The provider's Cargo feature is not compiled in, or the name is misspelt | Check the [provider page](providers/README.md) for the feature; in Docker, add it to `FEATURES` in the compose file |
| `validate-config` reports an unknown key | Every section rejects unknown keys | Provider-specific keys go in `[stt.options]` / `[tts.options]`; text processors are flat |
| `client_tools_path` is ignored | It was written after a `[table]` header, so TOML made it a key of that table | Move it above the first table |
| `phonemization failed: espeak not found` | Piper or Kokoro without eSpeak NG on `PATH` | Install `espeak-ng`, restart the terminal |
| Config with both `[realtime_provider]` and `[stt]` rejected | The two engines are mutually exclusive | Remove one |

## Audio and turn-taking

| Symptom | Cause | Where to look |
|---|---|---|
| Transcripts lose their first word ("o, how are you?") | The recogniser never saw the start of the utterance | Run the VAD probe on a recording: `speech_discarded_before_onset_ms` over ~250 ms means `[vad] pre_roll_ms` is too short. If the VAD is clear, probe the recogniser alone |
| Nothing is transcribed, or only dots | The browser is not delivering audio, or is delivering silence | The Playground's **Mic check**, which does not involve the pipeline; the browser console names a silent or frameless microphone after 1.5 s. A session that does commit on a wordless transcript is flagged `stt.transcript_empty` |
| Agent answers half-questions | Silence timeout too short | `[turn_detection] silence_timeout_ms`, or `mode = "smart"` |
| Long pause before every reply | Timeout too long, or the provider's endpointing is slow | `endpointing_ms` in the turn metrics; `provider_endpointing_ms` for speech-to-speech. The verdict names **turn detection** when the configured `silence_timeout_ms` is what the time was spent on |
| Every session grades `fair` on a pipeline that is slow on purpose | The built-in targets describe a general conversational agent | Set the pipeline's own [`[grading]`](configuration.md#grading-optional-advanced) rules; the verdict will say it was graded against them |
| Agent keeps talking after you interrupt | Barge-in not firing, or the client's playback buffer is deep | `[barge_in] speech_threshold` / `min_chunks`; the client must honour `clear_audio` and send `playback_report` |
| Interruptions fire on their own with speakers | Echo reaching the microphone | Use headphones, or keep echo gating on (`live` / `interactive` default) |
| No utterances at all with a cloud STT | `silence` turn detection with `passthrough` VAD has no silence to time | Use `mode = "provider"` or `"layered"` |
| Choppy playback after the first words | Audio leaves the worker slower than it plays | `output.emit_rtf` and `audio_post:*` budget ratios in the metrics; a sub-real-time TTS (VibeVoice) or an inline post-processor that cannot keep up |

## Sidecars and backends

| Symptom | Cause | Where to look |
|---|---|---|
| Client gets a `*_WARMUP_FAILED` warning right after `session_ready` | A backend (local TTS sidecar, `llama-server`, STT) is unreachable at session start | `GET /health` on the worker shows the component unhealthy; start the sidecar |
| First turn fails with a connection error (`TTS_SYNTHESIS_FAILED`, refused) | Same as above for a provider without a warm-up probe | The error message carries a "backend appears to be unreachable" hint |
| Tool calls never fire with a local model | `llama-server` without `--jinja` ignores the `tools` field | Add `--jinja` |
| AssemblyAI `1008 Too many concurrent sessions` | Account concurrency cap | The provider backs off; consider `persistent_session = true`; fewer workers per key |
| Google STT fails on first audio, not on connect | Headers are withheld until the first message | Check `location` (`us`/`eu` for `chirp_3`), credentials and the enabled API |
| STT worker ends with `gave up after N reconnect attempts` | The hosted provider accepted every connection and then failed each stream (a rejected model or sample rate, an account limit phrased as transient), or the link flapped more than 20 times in five minutes | The `reason` names the last error; fix that rather than raising `max_reconnect_attempts` |
| Gemini Live: long pause before speech | Dynamic thinking | 2.5: `thinking_budget = 0`; 3.1 Flash Live: `thinking_level = "minimal"` |

## Connections and fleet

| Symptom | Cause | Where to look |
|---|---|---|
| Client reaches `session_ready`, then silently disconnects, no error frame | Another process is bound to the same port (a leftover Docker worker on IPv6 `::1` while the native one is on IPv4); on Windows `localhost` resolves to `::1` first | `curl http://127.0.0.1:<port>/health` and `curl http://localhost:<port>/health`; compare `components`; check the port with `netstat` / `Get-NetTCPConnection` |
| Orchestrator shows zero workers | Worker cannot reach the orchestrator, or the orchestrator binds `127.0.0.1` inside Docker | `bind = "0.0.0.0"`; worker `[orchestrator] endpoint`; heartbeat logs |
| Dispatch hands out `127.0.0.1:8800` for every worker | Several containerised workers publish different host ports | Set `transport_advertise_address` per worker |
| Live parameter edits from the console fail with 502 | The worker registered an unreachable debug address | Set `advertise_address` to the compose service name |
| Port already in use (`os error 10048`) | A zombie worker | Stop it; give each worker its own `debug_port` |
| Worker log says `peer not reading: ws write timed out; closing session` | The client kept the TCP connection open but stopped reading (backgrounded tab, frozen process, NAT half-open) | Expected: the session is closed after `[transport] write_timeout_ms` (5 s) instead of holding its slot until the OS TCP timeout. Raise the value only for links with multi-second stalls |
| Realtime session freezes for a few seconds after a provider disconnect | The provider closed the connection or the transport failed; the runner backs off (1 s, doubling to 16 s within a streak) and reconnects (connect bounded at 10 s) | `realtime reconnect succeeded` / `RealtimeProviderError` in the log; three closes within 15 s or more than ten in a session stop reconnecting and end the session |
| Realtime session ends with `no frame from provider for 60 s` | The provider socket went half-open (host died without closing); the worker's pings got no pong | The runner reconnects; if it recurs, check the network path to the provider |

## Build

| Symptom | Fix |
|---|---|
| `cargo` not found after rustup | New terminal; `~/.cargo/bin` on `PATH` |
| Linker errors on Windows | Visual Studio Build Tools with the C++ workload, MSVC v143, Windows SDK |
| CUDA build fails | CUDA 12 toolkit and a developer environment; see [GPU and CUDA](gpu-and-cuda.md); default builds do not need CUDA |
| Docker build pinned to an old Rust | The workspace needs Rust 1.91; the Dockerfiles pin `rust:1.91` |

## Checking one component at a time

When a conversation goes wrong, the first question is which stage did it.
Each stage can be run alone against a known input, three ways:

- **Playground → Component check** in the admin console: shipped recordings
  or your own voice, a verdict per stage, the transcript and the synthesised
  audio to judge for yourself. See the
  [admin console guide](admin-console.md#playground).
- **`voice-pipes check`** from a terminal: builds the components from the
  config file, so nothing needs to be running, and exits non-zero when a
  stage fails, so it can gate a deploy. See the
  [CLI reference](../reference/cli.md#check--check-every-component).
- **`POST /debug/probe/{vad,stt,llm,tts}`** on a running worker's debug API.
  See the [HTTP API](../reference/http-api.md).

The shipped recordings (`voice-pipes/fixtures/`, `/app/fixtures` in the
images) each exist to show one failure: a sentence whose first word must
survive segmentation, two very short answers, a sentence with thinking
pauses, quiet speech near the detection floor, and silence.

## Reading a session

When none of the above matches, record the session (`[pipeline.audit]`),
open it in the admin console's Sessions page, and read the verdict and the
per-turn component table. The [observability guide](observability.md)
explains every field, and "Report for LLM" produces a text summary you can
paste into an issue.

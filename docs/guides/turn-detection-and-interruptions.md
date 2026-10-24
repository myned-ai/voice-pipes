# Turn detection and interruptions

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

Two settings decide whether a voice agent feels alive: how quickly it decides
you have finished talking, and how quickly it shuts up when you talk over it.
This page is the practical side. The state-machine reasoning behind it is in
[The interruption model](../explanation/interruption-model.md).

## Turn detection: "has the user finished?"

```toml
[turn_detection]
mode = "layered"
silence_timeout_ms = 700
```

| Mode | Decides on | Use when |
|---|---|---|
| `silence` | a fixed stretch of silence after speech (`silence_timeout_ms`, default 1000) | local STT with a real VAD (Silero or energy) |
| `provider` | the STT provider's own end-of-turn signal, with silence as fallback | cloud STT that endpoints itself: AssemblyAI, Google |
| `layered` | provider signal first, then the silence timeout | the wizard's default; safe with any STT |
| `smart` | a semantic end-of-turn model on the audio window, with a generous silence fallback | you want fewer mid-thought cut-offs and can spend tens of milliseconds of CPU |
| `vap` *(experimental)* | a model that predicts, from both speakers at once, who will hold the floor over the next two seconds | you want the pipeline to start work *before* the turn ends, and you are willing to measure it first |

`silence` needs a VAD that actually measures silence; with `passthrough` VAD
there is no silence to time, so use `provider` or `layered` there.

The first four modes all answer the same question — *has* the turn ended —
and can only answer it once it has. `vap` answers a different one, and the
difference is worth understanding before you pick between them.

### Smart Turn

`mode = "smart"` runs Pipecat's Smart Turn model (about 9 MB ONNX, CPU) on a
rolling window of the user's speech and commits the turn when it predicts
completion. The silence timeout becomes the hard fallback, so set it high.

```toml
[turn_detection]
mode = "smart"
silence_timeout_ms = 2000
smart_model_path = "models/smart-turn-v3.2-cpu.onnx"
smart_threshold = 0.5          # completion probability cut-off
smart_window_secs = 8          # the model was trained on 8 s windows
smart_min_audio_ms = 500       # do not run inference before this much speech
```

Requires the `turn-smart` feature of the CLI crate, which links the plugin
crate `voice-pipes-turn-smart`. The wizard downloads the model when you pick
this mode.

### VAP (anticipatory, experimental)

> **Experimental.** The mechanism works and is tested; what has not been
> established is how well the bundled model predicts turns *on your traffic*.
> The context window and threshold below are defaults chosen for cost, not
> values measured against real conversations, and the checkpoint is trained on
> English. Use it to evaluate, not to serve, until you have run the numbers
> yourself — see [Knowing whether it works](#knowing-whether-it-works).

`mode = "vap"` runs [Voice Activity Projection](https://github.com/ErikEkstedt/VAP)
(MIT), which predicts who will hold the floor over the next two seconds from
**both** channels — the user's microphone and what the agent is playing.

That second channel is the point. How the agent is speaking changes when the
user will stop, and from one channel alone a pause that invites a reply is not
distinguishable from one that does not. Because it predicts rather than
detects, it can be asked while the user is still talking, so the turn signal
carries a probability and a lead time on every evaluation instead of a verdict
after a pause has already been spent.

```toml
[turn_detection]
mode = "vap"
silence_timeout_ms = 2000       # hard fallback, as with `smart`
vap_model_path = "models/vap/vap-2s.onnx"
vap_threshold = 0.5             # confidence the user has yielded, to commit
vap_min_interval_ms = 200       # shortest gap between inferences
```

Requires the `turn-vap` feature of the CLI crate (plugin crate
`voice-pipes-turn-vap`; see
[`voice-pipes/examples/features/turn-vap.toml`](../../voice-pipes/examples/features/turn-vap.toml)
for a complete cascade pipeline config). The wizard does not download this
model; export it yourself:

```sh
git clone --depth 1 https://github.com/ErikEkstedt/VAP.git
uv venv --python 3.11 VAP/.venv
uv pip install --python VAP/.venv/bin/python torch torchaudio \
    --index-url https://download.pytorch.org/whl/cpu
uv pip install --python VAP/.venv/bin/python onnx onnxruntime einops
VAP/.venv/bin/python scripts/export-vap-onnx.py \
    --vap-root VAP --window-seconds 2 --out models/vap/vap-2s.onnx
```

**The context window is a property of the exported file, not of the config.**
A dynamic-length export bakes the attention mask at trace length and then
fails on every other input, so the window is chosen at export time and the
detector sizes its buffers to whatever it loads. Changing it means exporting
again. Cost is roughly linear in it, measured on CPU, with `%core` the share
of one core used at a 200 ms cadence:

| `--window-seconds` | ms/run | %core |
|---|---|---|
| 1 | 12.3 | 6% |
| 2 | 21.1 | 11% |
| 3 | 30.6 | 15% |
| 5 | 51.1 | 26% |
| 20 | 345.6 | 173% |

The checkpoint was trained with 20 s of context, which at 173% of a core is not
something you can run continuously; 2–3 s is. Shorter context is reported to
cost little accuracy, but that has not been measured here — treat 2 s as a
starting point and validate a window against your own traffic before trusting
it. Recording [continuous session audio](observability.md#continuous-session-audio)
gives you the material to do that.

Unlike `smart`, which only runs after a pause, `vap` runs *during* speech, so
its cost is bounded by `vap_min_interval_ms` rather than by a silence
threshold. Raise it if the CPU share above is more than you want to spend.

That also changes how it scales: `smart` is idle except after pauses, while
`vap` draws its share for the whole of every active turn. At 2 s and a 200 ms
cadence that is roughly a tenth of a core per concurrent session, so size for
it before running many sessions on one worker.

Both `serve` and `live` feed the user's microphone into channel 0 and the
agent's synthesized TTS audio into channel 1. While the user is speaking (or
when `[vad] mode = "passthrough"` streams continuous mic frames), channel 0
drives the stereo clock and overlapping agent audio writes onto the tail of
channel 1. Once a user turn commits (`turn_ended()`) and local VAD (`silero` or
`energy`) or echo-gating withholds silent mic frames while the agent speaks,
arriving agent TTS chunks advance both channels together (filling channel 0 with
silence) until user speech resumes.

#### Knowing whether it works

The bundled checkpoint is trained on
[Switchboard](https://catalog.ldc.upenn.edu/LDC97S62) — American English
telephone conversations between two people — with a CPC encoder pretrained on
English read speech. Two things follow, and neither is a reason not to use it,
only a reason to measure before trusting it:

- **Language.** Turn-taking cues are not language-neutral: pause length,
  final-syllable lengthening and the intonation before a yield all differ.
  A model that has not heard your language may still work, and may not.
- **Domain.** Switchboard is human talking to human. A voice agent is human
  talking to a machine, and people take turns differently with machines —
  less overlap, more deliberate pauses. This mismatch applies to *any*
  turn-taking model trained on human-human corpora, not just this one.

So treat the defaults as a starting point and check them against real
conversations. Recording
[continuous session audio](observability.md#continuous-session-audio) gives
you the material: both channels on one timeline, which is what any comparison
of turn detectors needs.

#### Other languages, and training your own

Two paths, in increasing order of effort:

1. **A multilingual checkpoint.** Multilingual VAP models exist
   ([Inoue et al., 2024](https://arxiv.org/abs/2403.06487)), and
   `scripts/export-vap-onnx.py --vap-root` takes any compatible checkout, so
   adopting one is an export rather than a code change. Confirm it on your own
   recordings afterwards rather than assuming.

2. **Fine-tuning on your own calls.** VAP's training target is a list of
   per-speaker speech intervals — no transcripts and no hand labelling — so
   the labels can be generated by running a voice activity detector over each
   channel of a recorded session offline, where it may use lookahead a live
   detector cannot. A recorded `session_audio.wav` is already in the shape the
   upstream training pipeline expects.

   That makes fine-tuning on your own deployment distribution feasible rather
   than theoretical, and the domain point above means it may help in English
   too, not only in a new language. Two caveats: the agent's channel is
   synthesised speech, so a model fitted to it learns *that voice's* rhythm and
   will want rechecking if you change TTS provider; and you cannot tell whether
   a fine-tune helped without a held-out set and a scorer, so build the
   measurement before the model.

### Tuning the silence timeout

- 450–700 ms feels snappy and suits short question-and-answer exchanges.
- 1000 ms (the default) is forgiving for people who pause mid-sentence.
- Under 400 ms the agent starts answering half-questions; go semantic
  (`smart`) instead of lower.

The per-turn metrics include `endpointing_latency_ms` (the time from the end
of speech to the committed utterance), so you can see what a setting costs.

## Barge-in: "did the user start talking over the agent?"

```toml
[barge_in]
enabled = true
min_chunks = 3
speech_threshold = 2000.0
```

A dedicated monitor watches microphone audio while the agent is speaking.
When RMS energy exceeds `speech_threshold` for `min_chunks` consecutive
chunks (about 20 ms each, so 3 is roughly 60 ms of sustained speech) it fires
an interrupt: in-flight LLM and TTS work is cancelled, the client is told to
flush its playback buffer (`clear_audio`), and the conversation history is
truncated to what was actually heard.

- Raise `speech_threshold` in noisy rooms; lower it for quiet speakers.
  The value is RMS on the 16-bit sample scale.
- Raise `min_chunks` if coughs and door slams trigger interruptions.
- Cloud STT and speech-to-speech providers that detect speech themselves
  also feed the monitor, so barge-in works with `passthrough` VAD.

### What the client must do

Barge-in is a cooperative protocol. The worker cannot know how much audio the
client has already played from its buffer, so the client reports it:

1. on `clear_audio`, stop playback immediately and drop queued audio for that
   `utterance_id`;
2. send `playback_report` with `last_played_offset_ms` (and optionally the
   text heard);
3. send `playback_complete` so the turn state returns to Listening.

The worker replies with `text_truncated` carrying the exact text the user
heard, which is what goes into the conversation history and the transcript
(via `truncate_last_assistant_message`). If the caller barges in **before** any
TTS audio has played (`Processing` state) or `playback_report` reports `0 ms`
played (`text = ""`), the worker rolls back the unspoken assistant entry
completely (`rollback_last_turn`) so the LLM never assumes the caller heard an
answer that never reached their speakers.
[Building a client](../clients/building-a-client.md) has the full sequence.

When the interrupting speech itself is not recognised (a short "no" or
"wait" that the recogniser finds no words in), no turn follows: the reply
was cut off and the worker goes back to listening. It sends
`utterance_dropped` so the client can say so; the session report raises
`stt.interruption_unheard` for it, since the VAD confirmed the caller was
talking and the recogniser missed it.

### Echo and open speakers

With open speakers the agent's own voice reaches the microphone and can trip
the monitor. The CLI's `live` and `interactive` modes apply client-side echo
gating by default (they drop microphone audio while playing); the browser
Playground relies on the browser's echo cancellation. With headphones you can
turn gating off (`--no-echo-gating`) for natural interruptions.

## Reading the result

Every interruption produces an `interrupted` event with its trigger
(`server_vad`, `client_cancel`, `early_barge`) and a `barge_in_resolved` event
with the resolved playback offset. The session view in the admin console
shows an orange divider at the point of interruption and the truncated text.
If the agent "keeps talking for a second" after you speak, look at the
barge-in round-trip metric: it separates detection latency from client
playback-buffer depth.

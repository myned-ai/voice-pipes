# Voice activity detection

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

VAD answers one question per audio chunk: is this speech? Voice Pipes uses
the answer in two places: to cut the microphone stream into utterances before
STT, and to detect the user speaking over the agent (barge-in). The design
that keeps those two uses independent is described in
[Composable VAD](../explanation/composable-vad.md); this page is the
configuration.

## Backends

```toml
[vad]
mode = "auto"
model = "models/silero_vad.onnx"
```

| `mode` | What it is | When to use |
|---|---|---|
| `auto` (default) | Silero if the binary links it and the model file exists, otherwise energy | the safe default |
| `silero` | Silero VAD, a small neural model run through ONNX Runtime | any local STT; robust to noise |
| `energy` | RMS amplitude threshold | last resort, quiet environments |
| `passthrough` | every chunk is speech | STT and speech-to-speech providers that run their own VAD and need continuous audio: AssemblyAI, Google, OpenAI Realtime, Gemini Live |

Silero is a plugin crate, `voice-pipes-vad-silero`, built into the CLI by
default (its `vad-silero` feature); it needs the model file, which the wizard
downloads. The binary names it the backend `auto` runs.

## Silero settings

```toml
[vad]
mode = "silero"
model = "models/silero_vad.onnx"
speech_prob = 0.5        # enter speech when probability >= this
neg_threshold = 0.35     # leave speech when probability < this
min_speech_ms = 250      # ignore bursts shorter than this
min_silence_ms = 100     # end an utterance only after this much silence
pre_roll_ms = 300        # audio kept from before the onset (the default)
```

The two thresholds form a hysteresis: a chunk has to be clearly speech to
start an utterance and clearly silence to end one, which stops the flapping a
single threshold produces around borderline probabilities. Setting
`neg_threshold` at or above `speech_prob` degenerates to a single threshold.

Silero runs on 512-sample windows at 16 kHz with a 64-sample context carried
between windows; other input rates are resampled internally. `speech_prob`
and `neg_threshold` are live-tunable from the admin console's parameter
editor without restarting the worker.

## Energy settings

```toml
[vad]
mode = "energy"
energy_threshold = 200.0   # RMS on the 16-bit sample scale
```

Also live-tunable. Energy VAD reports probability as 1.0 or 0.0.

## Input sample rate

Backends run at their own rate (Silero at 16 kHz). Audio arriving at any
other rate is resampled with the engine's windowed-sinc resampler, which
low-passes at the *target* Nyquist when downsampling, so a 48 kHz browser
capture reaches the model without high-frequency content folded into the
speech band. The Silero backend keeps a stateful resampler across chunks
(no seam every 20 ms). If the input rate changes mid-session the
segmentation task re-derives its 20 ms framing and silence budget and
resets the backend; the chunks it forwards to STT always carry the current
rate.

## Utterance timing versus chunk classification

`min_speech_ms` and `min_silence_ms` are consumed by the pipeline's
segmentation task, not by the backends: the backend labels chunks, the
pipeline decides when an utterance starts and ends. That is why they apply to
every backend, including energy.

**Pre-roll.** Speech is confirmed only after three consecutive speech
chunks, and a soft first syllable ("h" in "hello") scores under the
threshold to begin with, so by the time an utterance is declared its first
sounds are already in the past. The segmenter therefore keeps the last
`pre_roll_ms` of audio (300 ms unless set) and hands it to the recogniser in
front of the utterance. Too little and transcripts lose their first word;
more costs nothing but a little leading silence. To see what a setting does
to a real recording, run the VAD probe — it reports how much *speech* was
dropped before each onset (see
[Troubleshooting](troubleshooting.md#checking-one-component-at-a-time)).

Utterance end feeds [turn detection](turn-detection-and-interruptions.md);
VAD and turn detection are separate stages on purpose. A short pause can end
a VAD segment without ending the turn.

## Barge-in engines

While the agent speaks, a separate engine surface decides whether the user
started talking. The built-in engines are:

- `rms`: local RMS energy over incoming microphone audio, sub-20 ms reaction,
  configured by `[barge_in]`;
- `provider`: driven by the STT or realtime provider's own speech-started
  signal or its first partial transcript;
- `composite`: several engines raced, first to fire wins.

Which engines run is derived from the providers in use; you configure the
thresholds in `[barge_in]`, not the engines. See
[Turn detection and interruptions](turn-detection-and-interruptions.md#barge-in-did-the-user-start-talking-over-the-agent).

## Health

Every backend and engine is a component: a Silero model-load failure marks
`vad:silero` unhealthy on `/health` and in the admin console, and an inference
error records the error and degrades the component while the chunk falls back
to "not speech".

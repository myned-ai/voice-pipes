# Piper (TTS, local)

**Status:** active
**Last verified:** 2026-10-02
**Owner:** Docs

## What it is

Piper, a fast VITS-based neural text-to-speech, run in-process through ONNX
Runtime with eSpeak NG for phonemisation. Dozens of voices in many languages.
The default TTS provider and the fastest one.

## Cargo feature

`tts-piper` (in the default set), which links the plugin crate
`voice-pipes-tts-piper`. The binary registers it as the voice a pipeline
with no `[tts]` section runs. GPU: add `ort-cuda` and run with
`VP_ORT_EP=cuda`.

## Config

```toml
[tts]
provider = "piper"
model = "models/en_US-lessac-medium.onnx"   # the .onnx.json sibling must be next to it

[tts.options]
# pool_size = 2               # engines loaded; one per concurrent synthesis
```

Voices come from `rhasspy/piper-voices` on Hugging Face; each is an `.onnx`
plus an `.onnx.json` config. The wizard downloads `en_US-lessac-medium`.
Only single-speaker voices work: a voice whose `.onnx.json` states more than
one speaker (`num_speakers`) is refused at load with an error naming the
file, since there is no setting to pick a speaker. `tts.voice` is not read.

## Environment

`espeak-ng` on `PATH` at runtime (`winget install espeak-ng.espeak-ng`,
`apt install espeak-ng`, `brew install espeak-ng`). Without it every
synthesis fails with `phonemization failed: espeak not found`.
`VP_ORT_EP=cuda` runs the voice on the GPU in a build with `ort-cuda`;
asked for in a build without it, it stays on the CPU and the device report
says so.

## Capabilities

| | |
|---|---|
| Output sample rate | model-dependent, typically 22050 Hz |
| Streaming | Yes, frames are yielded per sentence or clause as inference completes |
| Word timestamps | No |
| Pooling | `pool_size` |
| Languages | per voice |

## Markdown Sanitization & Directives

Piper only synthesizes plain phonemic text; it does not parse SSML or inline Markdown.
- With Voice Pipes' dual-track sanitizer (`sanitize_markdown = true`, enabled by default), Markdown syntax (`**bold**`, `` `code` ``, `#`, etc.) is cleanly stripped before feeding Piper, preventing raw asterisks and backticks from being read aloud.
- Unsupported inline tags (e.g. `<break>` or emotion markers) are safely removed from the audio stream and recorded in turn diagnostics, so Piper never attempts to pronounce raw XML tags.
- Pronunciation substitutions configured in `[tts.lexicon]` (e.g. `"SQL" = "sequel"`) apply directly to Piper speech without altering display transcripts.

## Limitations

- Voice quality is serviceable, not expressive; [Kokoro](kokoro.md) is the
  usual upgrade when latency allows.
- Missing `model` is a configuration error at validation time, not a crash.

## Upstream

`github.com/rhasspy/piper`; voices at `huggingface.co/rhasspy/piper-voices`.

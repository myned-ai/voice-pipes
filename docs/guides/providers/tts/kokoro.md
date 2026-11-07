# Kokoro (TTS, local)

**Status:** active
**Last verified:** 2026-09-26
**Owner:** Docs

## What it is

Kokoro-82M, a StyleTTS 2 based model, run in-process through ONNX Runtime
with eSpeak NG for phonemisation. 24 kHz output, 26 voices, still faster than
real time on CPU. The usual choice when Piper's voice is not good enough.

It is a plugin crate, `voice-pipes-tts-kokoro`, built on `voice-pipes-sdk`;
the engine contains no Kokoro code. The `voice-pipes` binary links it behind
a feature and registers it at start, so configs select it exactly as before.

## Cargo feature

`tts-kokoro` on the **CLI crate**:
`cargo build -p voice-pipes-cli --features tts-kokoro`. Both worker images
carry it. For CUDA, add the CLI's `ort-cuda` and set `VP_ORT_EP=cuda`, as
for the other local models; `execution_provider = "cuda"` (or `"cpu"`)
under `[tts.options]`, when set, decides for Kokoro alone instead. Asked for
in a build without `ort-cuda`, it runs on the CPU and the device report says
so, naming the setting that asked. (Until 2026-09 it was an engine feature;
`-p voice-pipes --features tts-kokoro` no longer exists.)

## Config

```toml
[tts]
provider = "kokoro"
model = "models/kokoro-v1.0.onnx"
voices = "models/voices-v1.0.bin"
voice = "af_heart"

[tts.options]
# pool_size = 1
```

Files from the `kokoro-onnx` releases (`thewh1teagle/kokoro-onnx`,
Apache-2.0): the f32 model (~310 MB) and the voice pack (~27 MB). Smaller
`kokoro-v1.0.fp16.onnx` (169 MB) and `kokoro-v1.0.int8.onnx` (88 MB)
variants trade quality for speed; change `model` to use one.

```sh
curl -L -o models/kokoro-v1.0.onnx https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/kokoro-v1.0.onnx
curl -L -o models/voices-v1.0.bin  https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin
```

## Environment

`espeak-ng` on `PATH`, as for Piper.

## Capabilities

| | |
|---|---|
| Output sample rate | 24 kHz |
| Streaming | Yes |
| Per-request hints | voice, speed and language can be overridden per session or per request |
| Word timestamps | No |
| Pooling | `pool_size` |

## Inline Phoneme Notation & Lexicon

Kokoro natively supports inline International Phonetic Alphabet (IPA) phonemes using `[word](/ipa_phonemes/)` syntax (e.g. `[Kokoro](/kˈoʊ.kə.ɹoʊ/)`).
- The dual-track sanitizer preserves phoneme notation in `spoken_text` for synthesis while rendering plain display text (`Kokoro`) in user captions and conversation memory.
- You can also configure `[tts.lexicon]` in `pipeline.toml` to substitute words before phonemisation (e.g. `"SQL" = "sequel"`).

## Limitations

- English voices in the v1.0 pack; language coverage follows the model.
- Larger and slower than Piper; on a small CPU with several concurrent
  sessions raise `pool_size` or move it behind [gRPC split mode](../../../extending/grpc-split-mode.md).

## Upstream

`hexgrad/Kokoro-82M` on Hugging Face; ONNX export and voices at
`github.com/thewh1teagle/kokoro-onnx`.

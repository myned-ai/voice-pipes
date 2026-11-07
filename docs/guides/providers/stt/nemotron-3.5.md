# Nemotron 3.5 (STT, local, plugin)

**Status:** active
**Last verified:** 2026-09-23
**Owner:** Docs

## What it is

NVIDIA's `nemotron-3.5-asr-streaming-0.6b`, a cache-aware streaming
FastConformer with an RNNT decoder covering 40 locales, run in-process
through ONNX Runtime. It streams natively — each 320 ms chunk is encoded
against a cache of what came before, so interims arrive while the user
speaks and latency does not grow with utterance length — and the locale is
an input to the encoder, chosen per session.

It is the first recogniser in the repository built as a **plugin crate**:
`voice-pipes-stt-nemotron35` depends on `voice-pipes-sdk` and the shared
`voice-pipes-onnx` session pool, and the engine contains no code for it. The
`voice-pipes` binary links it behind a feature and registers its factory at
start; a custom binary does the same with one line. The
[plugin SDK guide](../../../extending/plugin-sdk.md#11-a-local-onnx-model-as-a-plugin-nemotron-35)
uses it as the worked example.

## Cargo feature

`stt-nemotron35` on the **CLI crate** (not the engine):
`cargo build -p voice-pipes-cli --features stt-nemotron35`.
`stt-nemotron35-cuda` adds the ONNX Runtime CUDA execution provider (select
it at runtime with `VP_ORT_EP=cuda`). Both Docker worker images carry it.

## Config

```toml
[stt]
provider  = "nemotron-3.5"                 # aliases: nemotron35, nemotron-3-5
model_dir = "models/nemotron-3.5"

[stt.options]
language  = "hr-HR"     # BCP-47 locale the model assumes; "auto" detects (default)
# pool_size = 2         # ONNX session sets loaded; one per concurrent session
# threads = 4           # intra-op threads per session; 0 lets ONNX Runtime choose
```

`model_dir` holds the export: `nemotron_onnx_config.json`, `tokens.txt`,
`decoder.onnx`, `joiner.onnx`, the two 320 ms encoder graphs and their
external weights (1.2 GB each, byte-identical), `LICENSE-OpenMDW-1.1` and
`NOTICE`:

```sh
python scripts/download_nemotron35.py
```

The script verifies every file's SHA-256 and fetches the shared weights once
(the second name is a hard link). No token and no NeMo environment is
needed. Two pinned sources: the default fetches this project's export from
Hugging Face (`myned-ai/nemotron-3.5-asr-streaming-0.6b-onnx`) file by file;
`--source community` fetches the community export
(`codavidgarcia/nemotron-3.5-asr-streaming-0.6b-onnx`). The two exports carry
byte-identical encoder weights and decode identically. The setup wizard
offers the provider and downloads it automatically when selected.

### Language

The locale is resolved in this order: the session's negotiated language
(what the client sends in `configure`), then `language` in the config, then
the model's own `auto`. A locale the model lacks is logged and the config
default is used; `voice-pipes validate-config` refuses one in the config.
`auto` costs accuracy on short utterances — a one-word "da" was decoded as
Russian in testing — so a pipeline for a known language should name it.

The transcript's `language` field carries the locale the model actually
used or detected, so a session under `auto` says what it heard.

### Chunk size

One size is shipped, 320 ms (the model offers 80–1120 ms; each size is its
own pair of 1.2 GB encoders). Latency falls and WER rises as the chunk
shrinks; 320 ms is the middle of that curve and the size the driver is
validated on, token for token, against the reference implementation.

## Environment

`VP_ORT_EP=cuda` runs the sessions on the GPU in a build with
`stt-nemotron35-cuda`; asked for in a build without it, the sessions stay on
the CPU and the device report says so.

## Capabilities

| | |
|---|---|
| Input sample rate | 16 kHz |
| Streaming | True streaming, chunked encoder with cache; interim results |
| Word timestamps | No |
| End-of-turn signal | No; use a real VAD with `silence` or `smart` turn detection |
| Language | 40 locales; per session; detection under `auto` |
| Reports | per-process warm-up, `concurrent_capacity = pool_size`, model identity, device |
| Metrics | Per-utterance `inference_count`, `algorithmic_delay_ms` (320 ms), `effective_latency_ms` (`delay * (1 + 1/rtfx)`), and audio-aligned `rtfx` (covering speech, tail silence, and boundary padding) |

## Limitations

- **Croatian is in the model's broad-coverage tier and it shows.** On the
  project's Croatian corpus (scripted and conversational, 18 clips) it
  scored 48.8% WER against Parakeet v3's 32.9%, and NVIDIA's own FLEURS
  figure for hr-HR at 320 ms is 25.8%. Google Chirp 3 remains the Croatian
  recommendation; `pipeline-croatian-nemotron35.toml` exists beside the
  Chirp and Parakeet pipelines so the three can be compared from the
  Playground, not as a replacement.
- **On English it is less accurate than its English-only predecessor**:
  5.0% WER against 3.3% on LibriSpeech at equal latency (the
  [Nemotron streaming](nemotron.md) page has the table). Use this model
  where its languages are the point; for English-only, run the other one.
- **Short interjections can decode to nothing.** A lone half-second word
  cut out by the VAD (a "no" or "okay" said to interrupt the agent) came
  back empty on a live session where Moonshine and Parakeet both heard a
  word in the same clip; replaying it offline with the previous utterance
  as context did not change that. The pipeline then drops the utterance,
  tells the client (`utterance_dropped`) and keeps the audio in the
  capture. A caller who interrupts with a full phrase is recognised
  normally. A one-word greeting as the first utterance of a session can go
  the same way: the same "Hello" decoded to nothing on a fresh stream and
  to "Hello" with a second more silence ahead of it, while the reference
  clip decoded worse with that same silence, so neither priming the engine
  nor a longer pre-roll is a fix. The Playground shows the drop; saying the
  greeting again, or a full sentence, is recognised.
- **A question's closing mark comes late.** The model decides on a "?"
  only once it hears what follows, so a question ended by silence comes
  back without it and the mark is emitted when the next utterance starts.
  The provider drops a closing mark that opens an utterance; the question
  itself is committed without it, which the LLM does not need.
- fp16 weights, fp32 activations: ONNX Runtime inserts casts on CPU, so CPU
  decoding is around real time with four threads (988 ms for 1.9 s of
  audio in a batch call, including the first-chunk cost). The CUDA build is
  the intended deployment for more than one session per worker.
- Each session holds one set of ONNX sessions for its whole life. The
  provider loads `pool_size` sets (default 2); a session beyond that waits
  up to 5 s for a free set and then fails with an error that says so. Each
  set is a full copy of the model in memory (about 1.3 GB), so size the pool
  to the sessions per worker, not higher. The worker warms one set at start
  and warns when `pool_size` is below the sessions it is configured to
  serve.
- The model emits a word only once it has seen silence after it, so the
  provider appends 400 ms of silence to every utterance before closing it
  (one or two extra encoder chunks). Without it a one-word turn such as
  "yes" decodes to nothing, and the community reference engine, which
  flushes only the partial last chunk, drops it too.
- **Emission can lag by seconds on difficult audio.** On a noisy or
  low-level microphone the model holds its tokens back until it is sure
  and then emits them in a burst — measured 2–3 s after the words on a
  laptop microphone where Parakeet decoded the same segments at once, and
  at some utterance boundaries not at all. The provider therefore keeps
  one engine per session and carries its caches across utterances (the
  model's own streaming mode; the stream then decodes exactly as one
  batch pass would), cutting only the token stream at a VAD boundary.
  Words that arrive after their boundary are attributed to the next
  utterance rather than lost, so a late "Horse," opens the following
  turn's transcript. A clean microphone does not show this; a poor one
  makes Parakeet the better English choice.

## Upstream

NVIDIA NeMo; model card `nvidia/nemotron-3.5-asr-streaming-0.6b`
(OpenMDW-1.1, ungated). This project's export (hosted on Hugging Face at
`myned-ai/nemotron-3.5-asr-streaming-0.6b-onnx`) is made from that checkpoint with the
community tooling of `codavidgarcia/nemotron-3.5-asr-streaming-onnx`
(Apache-2.0), vendored under `scripts/nemotron35/`; the
community's own export `codavidgarcia/nemotron-3.5-asr-streaming-0.6b-onnx`
is available via `--source community` and is the reference the plugin's
token tests are held to. The English-only
predecessor, `nemotron-speech-streaming-en-0.6b`, is the built-in
[Nemotron streaming](nemotron.md) provider.

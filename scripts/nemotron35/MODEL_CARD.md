---
license: other
license_name: openmdw-1.1
license_link: https://openmdw.ai/license/1-1/
base_model: nvidia/nemotron-3.5-asr-streaming-0.6b
pipeline_tag: automatic-speech-recognition
library_name: onnx
language:
  - en
  - es
  - de
  - fr
  - it
  - ar
  - ja
  - ko
  - pt
  - ru
  - hi
  - zh
  - vi
  - he
  - nl
  - cs
  - da
  - pl
  - no
  - sv
  - th
  - tr
  - bg
  - el
  - et
  - fi
  - hr
  - hu
  - lt
  - lv
  - ro
  - sk
  - uk
  - mt
  - sl
datasets:
  - nvidia/Granary
  - multilingual_librispeech
  - fleurs
  - mozilla-foundation/common_voice_8_0
  - voxpopuli
  - europarl
tags:
  - onnx
  - onnxruntime
  - fp16
  - streaming-asr
  - cache-aware-asr
  - automatic-speech-recognition
  - speech-recognition
  - FastConformer
  - RNNT
  - nemotron3_5_asr
  - multilingual
  - voice-pipes
metrics:
  - wer
model-index:
  - name: nemotron-3.5-asr-streaming-0.6b-onnx
    results:
      - task:
          name: Automatic Speech Recognition
          type: automatic-speech-recognition
        dataset:
          name: FLEURS (English, upstream checkpoint @ 1.12s)
          type: google/fleurs
          config: en_us
          split: test
        metrics:
          - name: WER (upstream 1.12s frame size, LangID)
            type: wer
            value: 7.91
            verified: false
      - task:
          name: Automatic Speech Recognition
          type: automatic-speech-recognition
        dataset:
          name: FLEURS (Spanish, upstream checkpoint @ 1.12s)
          type: google/fleurs
          config: es_419
          split: test
        metrics:
          - name: WER (upstream 1.12s frame size, LangID)
            type: wer
            value: 4.11
            verified: false
      - task:
          name: Automatic Speech Recognition
          type: automatic-speech-recognition
        dataset:
          name: FLEURS (French, upstream checkpoint @ 1.12s)
          type: google/fleurs
          config: fr_fr
          split: test
        metrics:
          - name: WER (upstream 1.12s frame size, LangID)
            type: wer
            value: 9.03
            verified: false
      - task:
          name: Automatic Speech Recognition
          type: automatic-speech-recognition
        dataset:
          name: FLEURS (German, upstream checkpoint @ 1.12s)
          type: google/fleurs
          config: de_de
          split: test
        metrics:
          - name: WER (upstream 1.12s frame size, LangID)
            type: wer
            value: 8.31
            verified: false
---

# Nemotron 3.5 ASR Streaming 0.6B — ONNX (320 ms, FP16 Encoder)

This repository hosts a reproducible, hash-verified ONNX export of NVIDIA's [`nvidia/nemotron-3.5-asr-streaming-0.6b`](https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b) (checkpoint revision `ea30d66debe3740a08b573244286791d423d6b3e`), built and maintained by the [Voice Pipes](https://github.com/myned-ai/voice-pipes) project (`r2`, 2026-09-23).

Nemotron 3.5 ASR Streaming 0.6B is a cache-aware streaming FastConformer-RNNT speech recognition model covering 40+ languages/locales with per-session language conditioning and automatic language detection.

---

## Why This Export Exists & What Changed

Compared to the upstream PyTorch/NeMo checkpoint and multi-chunk community ONNX mirrors, this release is tailored for production voice pipelines:

1. **Single 320 ms Streaming Chunk Size (~1.3 GB total)**:
   The upstream model supports five chunk sizes (80, 160, 320, 560, and 1120 ms), each requiring its own pair of ~1.2 GB encoder weight files. This repository ships only the **320 ms** configuration (`lookahead = 3`, `frame_ms = 80.0`), which sits in the sweet spot of the latency-vs-accuracy curve and keeps the total download to ~1.3 GB.
2. **Mixed-Precision Layout (FP16 Encoder Weights, FP32 I/O & RNNT Head)**:
   - `encoder_320ms_first_fp16.onnx` and `encoder_320ms_fp16.onnx` have their weights cast to **FP16** using the >2 GiB-safe converter (`quantize.py`), cutting encoder weight storage in half (~2.4 GB → ~1.2 GB) while keeping all graph inputs and outputs in **FP32**.
   - `decoder.onnx` and `joiner.onnx` remain in **FP32** for numerical stability and exact token-for-token parity with the reference implementation.
3. **Reproducible, Pinned, Hash-Verified Build**:
   Exported from the ungated NVIDIA checkpoint using vendored Apache-2.0 tooling from [`codavidgarcia/nemotron-3.5-asr-streaming-onnx`](https://github.com/codavidgarcia/nemotron-3.5-asr-streaming-onnx) (`fe19096c852a957af1362c8facb2957b8f606c57`) via [`scripts/nemotron35/make_export.py`](https://github.com/myned-ai/voice-pipes/tree/main/scripts/nemotron35). Every release is gated by ONNX-vs-PyTorch parity checks and Rust token-identity tests in `voice-pipes-stt-nemotron35`, and every file's SHA-256 is recorded in `SHA256SUMS`.
4. **Self-Hosted Availability**:
   Provides a pinned, token-free source for Voice Pipes CLI wizards, Docker worker builds, and CI without requiring a NeMo/PyTorch export environment.

> **Note on external encoder weights:** `encoder_320ms_first_fp16.onnx.data` and `encoder_320ms_fp16.onnx.data` are byte-identical (`1,228,512,000` bytes, SHA-256 `159887b78f1e36fd59f5cf17be810fd61e98a96b61673b6de0dbe2b8dfb47a31`). Because ONNX Runtime resolves each graph's external weights by filename, both filenames are provided in this repository for `huggingface_hub` compatibility. When using Voice Pipes' [`download_nemotron35.py`](https://github.com/myned-ai/voice-pipes/blob/main/voice-pipes/scripts/download_nemotron35.py), the 1.2 GB weights file is downloaded once and hard-linked to the second name.

---

## Repository Contents

| File | Size | Precision | Role |
|---|---:|---|---|
| `encoder_320ms_first_fp16.onnx` | 668 KB | FP16 weights / FP32 I/O | First-chunk FastConformer encoder graph (320 ms) |
| `encoder_320ms_first_fp16.onnx.data` | 1.23 GB | FP16 | External weights for first-chunk encoder |
| `encoder_320ms_fp16.onnx` | 611 KB | FP16 weights / FP32 I/O | Steady-state FastConformer encoder graph (320 ms) |
| `encoder_320ms_fp16.onnx.data` | 1.23 GB | FP16 | External weights for steady-state encoder (byte-identical to above) |
| `decoder.onnx` | 61.4 MB | FP32 | 2-layer LSTM RNNT prediction network (`hidden_size = 640`) |
| `joiner.onnx` | 33.6 MB | FP32 | RNNT joint network (`vocab_size = 13088`, `blank_token_id = 13087`) |
| `nemotron_onnx_config.json` | 8.3 KB | — | Model metadata, `prompt_dictionary`, mel feature config, and `cache_shapes` |
| `tokens.txt` | 145 KB | — | SentencePiece vocabulary (`13,088` entries) |
| `SHA256SUMS` | 1 KB | — | SHA-256 checksums for all artifacts |
| `LICENSE-OpenMDW-1.1` | 2.6 KB | — | NVIDIA OpenMDW License Agreement v1.1 |
| `NOTICE` | 1.0 KB | — | Provenance, attribution, and third-party notices |

---

## How to Use

### 1. With Voice Pipes (Recommended)

Voice Pipes supports this model natively via the `voice-pipes-stt-nemotron35` plugin crate (`provider = "nemotron-3.5"`).

**Download via the setup wizard** (auto-downloads when selecting Nemotron 3.5) or fetch manually with SHA-256 verification:

```sh
python voice-pipes/scripts/download_nemotron35.py
```

**Configure in `pipeline.toml`:**

```toml
[stt]
provider  = "nemotron-3.5"
model_dir = "models/nemotron-3.5"

[stt.options]
language  = "en-US"   # BCP-47 locale from prompt_dictionary, or "auto" (default)
# pool_size = 2       # Loaded ONNX session sets (1 per concurrent stream, ~1.3 GB each)
# threads   = 4       # Intra-op threads per session (0 = ORT default)
```

For GPU inference, build with `stt-nemotron35-cuda` and set `VP_ORT_EP=cuda`.

### 2. Direct Download (`huggingface_hub`)

```sh
hf download myned-ai/nemotron-3.5-asr-streaming-0.6b-onnx --local-dir ./nemotron-3.5-onnx
cd nemotron-3.5-onnx
sha256sum -c SHA256SUMS
```

Or in Python:

```python
from huggingface_hub import snapshot_download

model_dir = snapshot_download("myned-ai/nemotron-3.5-asr-streaming-0.6b-onnx")
```

### 3. Standalone ONNX Runtime Inference (Python)

The model expects **16 kHz, 16-bit mono PCM** audio converted to **128-bin Slaney log-mel spectrograms** (`n_fft=512`, `hop_length=160`, `win_length=400`, `preemphasis=0.97`, `fmin=0.0`, `fmax=8000.0`, `log_zero_guard=5.960464477539063e-08`).

Each 320 ms chunk feeds the encoder (`encoder_320ms_first_fp16.onnx` for chunk 0, `encoder_320ms_fp16.onnx` for subsequent chunks) along with `prompt_ids` (from `prompt_dictionary` in `nemotron_onnx_config.json`), `cache_mask`, and the 75 recurrent cache tensors (`k_cache_0..23`, `v_cache_0..23`, `conv2d_cache_0..2`, `conv1d_cache_0..23`), followed by greedy RNNT decoding with `decoder.onnx` and `joiner.onnx`:

```python
import json
from pathlib import Path
import numpy as np
import onnxruntime as ort

model_dir = Path("./nemotron-3.5-onnx")
cfg = json.loads((model_dir / "nemotron_onnx_config.json").read_text(encoding="utf-8"))

providers = ["CUDAExecutionProvider", "CPUExecutionProvider"]
enc_first = ort.InferenceSession(str(model_dir / "encoder_320ms_first_fp16.onnx"), providers=providers)
enc_step = ort.InferenceSession(str(model_dir / "encoder_320ms_fp16.onnx"), providers=providers)
decoder = ort.InferenceSession(str(model_dir / "decoder.onnx"), providers=providers)
joiner = ort.InferenceSession(str(model_dir / "joiner.onnx"), providers=providers)

# Select language prompt ID (e.g., "en-US" -> 0, "hr-HR" -> 29, "auto" -> 101)
prompt_id = np.array([cfg["prompt_dictionary"]["en-US"]], dtype=np.int64)

# Initialize encoder caches from cfg["cache_shapes"]
caches = {
    name: np.zeros(shape, dtype=np.float32)
    for name, shape in cfg["cache_shapes"].items()
}

# Initialize RNNT decoder state: [num_decoder_layers, batch=1, decoder_hidden_size]
h = np.zeros((cfg["num_decoder_layers"], 1, cfg["decoder_hidden_size"]), dtype=np.float32)
c = np.zeros((cfg["num_decoder_layers"], 1, cfg["decoder_hidden_size"]), dtype=np.float32)
last_token = np.array([[cfg["blank_token_id"]]], dtype=np.int64)
```

For a complete end-to-end streaming driver (including mel feature extraction, chunk framing, and RNNT decoding), see:
- Rust: [`plugins/stt/nemotron35/src/`](https://github.com/myned-ai/voice-pipes/tree/main/plugins/stt/nemotron35/src)
- Python reference exporter/validator: [`scripts/nemotron35/vendor/export_onnx.py`](https://github.com/myned-ai/voice-pipes/blob/main/scripts/nemotron35/vendor/export_onnx.py)

---

## Supported Languages & Prompt IDs

`nemotron_onnx_config.json` maps BCP-47 locales (and short aliases) to `prompt_ids`. Specifying an explicit locale is recommended for short utterances, as `"auto"` (`101`) can misclassify brief single-word turns.

- **Auto-detect:** `auto` (`101`, `default_prompt_id`)
- **Primary & European / Asian / Middle Eastern locales:** `en-US` (`0`), `en-GB` (`1`), `es-ES` (`2`), `es-US` (`3`), `zh-CN` (`4`), `zh-TW` (`5`), `hi-IN` (`6`), `ar-AR` (`7`), `fr-FR` (`8`), `fr-CA` (`100`), `de-DE` (`9`), `ja-JP` (`10`), `ru-RU` (`11`), `pt-BR` (`12`), `pt-PT` (`13`), `ko-KR` (`14`), `it-IT` (`15`), `nl-NL` (`16`), `pl-PL` (`17`), `tr-TR` (`18`), `uk-UA` (`19`), `ro-RO` (`20`), `el-GR` (`21`), `cs-CZ` (`22`), `hu-HU` (`23`), `sv-SE` (`24`), `da-DK` (`25`), `fi-FI` (`26`), `no-NO` (`27`), `nb-NO` (`103`), `nn-NO` (`104`), `sk-SK` (`28`), `hr-HR` (`29`), `bg-BG` (`30`), `lt-LT` (`31`), `th-TH` (`32`), `vi-VN` (`33`), `id-ID` (`34`), `ms-MY` (`35`), `bn-IN` (`36`), `ur-PK` (`37`), `fa-IR` (`38`), `ta-IN` (`39`), `te-IN` (`40`), `mr-IN` (`41`), `gu-IN` (`42`), `kn-IN` (`43`), `ml-IN` (`44`), `si-LK` (`45`), `ne-NP` (`46`), `km-KH` (`47`), `sw-KE` (`48`), `am-ET` (`49`), `ha-NG` (`50`), `zu-ZA` (`51`), `yo-NG` (`52`), `ig-NG` (`53`), `af-ZA` (`54`), `rw-RW` (`55`), `so-SO` (`56`), `ny-MW` (`57`), `ln-CD` (`58`), `or-KE` (`59`), `et-EE` (`60`), `lv-LV` (`61`), `sl-SI` (`62`), `he-IL` (`64`), `ku-TR` (`65`), `az-AZ` (`66`), `ka-GE` (`67`), `hy-AM` (`68`), `uz-UZ` (`69`), `tg-TJ` (`70`), `ky-KG` (`71`), `qu-PE` (`80`), `ay-BO` (`81`), `gn-PY` (`82`), `nah-MX` (`83`), `mi-NZ` (`96`), `haw-US` (`97`), `sm-WS` (`98`), `to-TO` (`99`), `mt-MT` (`102`).

---

## Operational Notes & Limitations

- **Trailing silence for short turns:** The streaming RNNT emits tokens only after observing trailing context. Append ~400 ms of silence at end-of-utterance before closing a stream so short single-word turns (e.g., *"yes"*, *"hello"*) are not dropped.
- **CPU vs. CUDA performance:** Because the encoder stores weights in FP16 with FP32 activations, ONNX Runtime inserts type-cast nodes on CPU (~1x real-time with 4 threads). For multi-session production concurrency, use `CUDAExecutionProvider`.
- **Memory per session:** Each loaded session set holds ~1.3 GB in memory/VRAM.
- **Accuracy vs. chunk size:** At the 320 ms chunk size, English WER is higher than NVIDIA's English-only streaming model (5.0% vs 3.3% on LibriSpeech); use this model when multilingual coverage is needed. See the [Voice Pipes Nemotron 3.5 provider guide](https://github.com/myned-ai/voice-pipes/blob/main/docs/guides/providers/stt/nemotron-3.5.md) for detailed empirical notes.

---

## License & Attribution

- **Model Weights:** Distributed under the **NVIDIA OpenMDW License Agreement v1.1** (`LICENSE-OpenMDW-1.1`, <https://openmdw.ai/license/1-1/>).
  Attribution: *"NVIDIA Nemotron 3.5 ASR (`nvidia/nemotron-3.5-asr-streaming-0.6b`), NVIDIA Corporation"*.
  This export is not affiliated with, sponsored by, or endorsed by NVIDIA.
- **Export & Quantization Tooling:** Apache-2.0, vendored from [`codavidgarcia/nemotron-3.5-asr-streaming-onnx`](https://github.com/codavidgarcia/nemotron-3.5-asr-streaming-onnx) at commit `fe19096c852a957af1362c8facb2957b8f606c57`.

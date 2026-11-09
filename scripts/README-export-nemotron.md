# Exporting Nemotron-Speech-Streaming to ONNX

> **Nemotron 3.5 needs none of this.** The multilingual successor ships as a
> ready ONNX export fetched by `download_nemotron35.py` — no NeMo, no token,
> no export run — and is served by the `voice-pipes-stt-nemotron35` plugin;
> see `docs/guides/providers/stt/nemotron-3.5.md`. This chapter remains for
> the English-only `nemotron-speech-streaming-en-0.6b` built-in provider,
> which is the more accurate of the two on English
> (measured at 3.3% LibriSpeech WER).

This document describes how to produce the cache-aware streaming ONNX graphs
consumed by [`examples/streaming_decode`](../plugins/stt/nemotron/examples/streaming_decode/main.rs) of voice-pipes-stt-nemotron
from NVIDIA's published `.nemo` checkpoint.

---

## 1. Source checkpoint

| Field | Value |
|-------|-------|
| Name  | `nvidia/nemotron-speech-streaming-en-0.6b` |
| URL   | https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b |
| File  | `nemotron-speech-streaming-en-0.6b.nemo` (~2.47 GB) |
| Precision | FP32 |
| License | NVIDIA Open Model License (see §5 below) |

The checkpoint was gated when this chapter was written; as of 2026-09 it is
public, and the download needs no token or license click. The steps below
still work with a token set.

### 1a. Download with the Hugging Face CLI (Windows)

Install the CLI via winget (no Python needed):

```powershell
winget install --id Hugging-Face.HuggingFaceCLI
```

If winget isn't available, the official one-liner works in Windows
PowerShell 5.1 **as long as you force TLS 1.2 first** (5.1 defaults to
TLS 1.0/1.1, which HF rejects with *"The underlying connection was
closed"*):

```powershell
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
irm https://hf.co/cli/install.ps1 | iex
```

Then authenticate (paste a read token from <https://huggingface.co/settings/tokens>)
and download. Note the CLI binary is now `hf` (the legacy `huggingface-cli`
name was removed in `huggingface_hub` 0.34+):

```powershell
hf auth login
hf download nvidia/nemotron-speech-streaming-en-0.6b `
    --local-dir $env:USERPROFILE\hf-cache\nemotron-streaming
```

### 1b. Alternative: pip install the CLI

If winget can't find the package or you prefer pip:

```powershell
python -m pip install --user --upgrade "huggingface_hub[cli]"
hf auth login
hf download nvidia/nemotron-speech-streaming-en-0.6b `
    --local-dir $env:USERPROFILE\hf-cache\nemotron-streaming
```

### 1c. Alternative: direct `curl.exe`

You only need one file. With a read token in `$token`:

```powershell
$dir = "$env:USERPROFILE\hf-cache\nemotron-streaming"
New-Item -ItemType Directory -Force $dir | Out-Null
$url = "https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b/resolve/main/nemotron-speech-streaming-en-0.6b.nemo"
curl.exe -L -H "Authorization: Bearer $token" -o "$dir\nemotron-speech-streaming-en-0.6b.nemo" $url
```

> If `Invoke-WebRequest` / `irm` fails with *"The underlying connection was
> closed"*, that's Windows PowerShell 5.1 defaulting to TLS 1.0/1.1. Set
> `[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12`
> first (see §1a), or use `curl.exe` (above), or use PowerShell 7 (`pwsh`).

---

## 2. Pinned environment

NeMo's export path is fragile across versions and pulls in ~4 GB of CUDA /
torch / numpy wheels. **The recommended path is the Docker image** — it
freezes the env, runs CPU-only, and works identically on Windows, macOS,
and Linux without polluting your local Python.

### 2a. Recommended: Docker (PowerShell wrapper)

```powershell
# One-shot wrapper. Builds the image on first use, then runs the export.
.\voice-pipes\scripts\export-nemotron.ps1 `
    -NemoFile $env:USERPROFILE\hf-cache\nemotron-streaming\nemotron-speech-streaming-en-0.6b.nemo
```

Outputs land in `voice-pipes/models/nemotron-streaming/<config>/`. Useful
flags:

| Flag | Meaning |
|------|---------|
| `-Config '7,10,7'` (repeatable) | Limit to specific streaming configs. |
| `-SkipMel`       | Don't export the mel preprocessor. |
| `-Opset 17`      | ONNX opset (default 17). |
| `-OutDir <path>` | Override output directory. |
| `-Rebuild`       | Force `docker build` even if image exists. |

What the wrapper does is just `docker build` + `docker run`; if you'd
rather invoke Docker directly:

```powershell
docker build -f scripts/Dockerfile.nemo-export `
             -t voice-pipes/nemo-export:1 `
             scripts

docker run --rm `
    -v ${PWD}/voice-pipes/models/nemotron-streaming:/out `
    -v $env:USERPROFILE/hf-cache/nemotron-streaming:/in:ro `
    voice-pipes/nemo-export:1 `
    --nemo-file /in/nemotron-speech-streaming-en-0.6b.nemo `
    --out-dir   /out
```

The image is CPU-only (CPU torch wheels, `CUDA_VISIBLE_DEVICES=""`). Export
takes a couple of minutes per streaming config on a modern laptop.

### 2b. Alternative: native venv

Use this only if Docker isn't an option (e.g. CI without DinD). Tested on
Windows 11 + WSL2 Ubuntu 22.04 and on bare Ubuntu 22.04. CUDA is required
by `restore_from`'s default code path even though export itself runs on
CPU — bypass with `CUDA_VISIBLE_DEVICES=""` per §6 troubleshooting.

```bash
# Fresh venv
python3.10 -m venv .venv-nemo-export
source .venv-nemo-export/bin/activate

pip install --upgrade pip wheel

# Pinned core deps
pip install \
    'torch==2.4.1' \
    'torchaudio==2.4.1' \
    'numpy<2.0' \
    'onnx==1.17.0' \
    'onnxruntime==1.19.2' \
    'sentencepiece==0.2.0'

# NeMo — pinned to a release that ships the cache-aware streaming export.
pip install 'nemo_toolkit[asr]==2.0.0'
```

If `nemo_toolkit==2.0.0` won't resolve in your Python version, fall back to
`nemo_toolkit[asr]==1.23.0` and re-run; both ship the cache-aware streaming
encoder export but `set_default_att_context_size` lives in slightly
different places. The script's `configure_streaming` helper handles both.

> **Why a separate venv?** Voice-pipes is a Rust project; we only touch
> Python during export. Keeping this venv outside the repo (or in a
> gitignored path) prevents accidental commit of a 4 GB site-packages tree.

---

## 3. What gets exported

Whether invoked via Docker (§2a) or native venv (§2b), the script exports
**both** paper Table 5 streaming configurations by default:

| Config (`left,chunk,right`) | Chunk | History | Algorithmic delay | Reference WER |
|-----------------------------|-------|---------|-------------------|---------------|
| `7,10,7` | 560 ms | 5.6 s | 560 ms | 8.20% |
| `2,35,2` | 160 ms | 28 s  | 160 ms | 8.95% |

Output layout:

```
voice-pipes/models/nemotron-streaming/
    7-10-7/
        encoder.onnx
        decoder_joint.onnx        # or decoder.onnx + joint.onnx (older NeMo)
        nemo_mel.onnx
        tokenizer.model
        export_metadata.json
    2-35-2/
        encoder.onnx
        ...
```

Native-venv invocation (skip if you used the Docker path):

```bash
cd scripts
python export_nemotron_streaming.py \
    --nemo-file ~/Downloads/nemotron-speech-streaming-en-0.6b.nemo \
    --out-dir   ../models/nemotron-streaming \
    --opset     17
```

To export only one config (Docker or native):

```powershell
# Docker
.\voice-pipes\scripts\export-nemotron.ps1 `
    -NemoFile <path-to-.nemo> -Config '7,10,7'
```
```bash
# Native
python export_nemotron_streaming.py \
    --nemo-file ~/Downloads/nemotron-speech-streaming-en-0.6b.nemo \
    --out-dir   ../models/nemotron-streaming \
    --config    7,10,7
```

---

## 4. Verifying the export

`export_metadata.json` records each ONNX graph's input/output names and
shapes. Open it after the script finishes and confirm:

- The encoder lists cache tensors as **named graph inputs** (typically
  `cache_last_channel`, `cache_last_time`, and corresponding length
  inputs) and **named graph outputs** with `_next` suffixes. If they are
  missing, the export collapsed the cache into the model state and we need
  a different code path — surface this in the Phase 1 spike notes
  (see PLAN open question #1).
- The mel preprocessor (if exported) has a single waveform input and a
  log-mel output of shape `[B, mel_bins, T]`. Compare `mel_bins` against
  the existing `models/parakeet-tdt/nemo128.onnx` (128) — if they match
  *and* normalization semantics match, the Parakeet preprocessor is
  reusable; otherwise keep the Nemotron-specific one.

A quick smoke test from Python:

```python
import onnxruntime as ort
sess = ort.InferenceSession("../models/nemotron-streaming/7-10-7/encoder.onnx")
for i in sess.get_inputs():  print("in ", i.name, i.shape)
for o in sess.get_outputs(): print("out", o.name, o.shape)
```

---

## 5. License & attribution

The Nemotron checkpoint is distributed under the **NVIDIA Open Model License
Agreement**. Read the license shipped inside the `.nemo` zip (`LICENSE` file
at the archive root) before redistributing the exported ONNX. Key obligations
relevant to this project:

- Attribution must be preserved in any derivative artifact.
- The exported ONNX inherits the source license — do **not** commit the
  exported files into this repository under any other license.
- If we ship binaries that bundle the model, add an attribution notice
  pointing back to the HuggingFace card.

The recommended distribution model for now: users export their own ONNX
locally and place it under `voice-pipes/models/nemotron-streaming/`. The
spike example at [`examples/streaming_decode`](../plugins/stt/nemotron/examples/streaming_decode/main.rs) of voice-pipes-stt-nemotron
takes the model directory as a CLI argument — nothing in the repo assumes a
checked-in model.

---

## 6. Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `restore_from` hangs allocating CUDA | NeMo defaults to GPU on import | Set `CUDA_VISIBLE_DEVICES=` before running |
| `RuntimeError: ONNX export failed: dynamic axes` | Older opset | Re-run with `--opset 17` (default) |
| Encoder ONNX has no cache I/O | `setup_streaming_params` not applied | Confirm NeMo version ≥ 1.23; check `configure_streaming` did not silently no-op |
| Tokenizer copy warning | `model.tokenizer.model_path` not exposed | Manually unzip the `.nemo` and copy `tokenizer.model` into the output dir |

---

## 7. After export

Proceed to Phase 1 step 1.3:

```bash
cargo run -p voice-pipes-stt-nemotron --example streaming_decode -- \
    --model-dir voice-pipes/models/nemotron-streaming/7-10-7 \
    --wav       voice-pipes/examples/audio/<your-clip>.wav
```

# VibeVoice-Realtime TTS Sidecar

Local OpenAI-compatible TTS server using Microsoft's [VibeVoice-Realtime-0.5B](https://huggingface.co/microsoft/VibeVoice-Realtime-0.5B).

## Requirements

- **Python 3.12** (Recommended on Windows for stable CUDA wheels)
- **NVIDIA GPU** with CUDA support (RTX 3070+ recommended, ~2.7 GB VRAM footprint)
- **NVIDIA CUDA Toolkit** installed

## Setup

Follow these steps from the repository root:

```powershell
# 1. Create a virtual environment using Python 3.12
py -3.12 -m venv .venv-vibevoice

# 2. Install CUDA-compatible PyTorch and torchaudio wheels
.venv-vibevoice\Scripts\pip.exe install torch torchaudio --index-url https://download.pytorch.org/whl/cu124

# 3. Install other requirements (FastAPI, uvicorn, diffusers, scipy, numba, etc.)
.venv-vibevoice\Scripts\pip.exe install -r scripts/vibevoice/requirements.txt

# 4. Install the vendored VibeVoice package as an editable package
.venv-vibevoice\Scripts\pip.exe install -e vendor/vibevoice
```

## Downloading Assets

Run the download scripts from the repository root to fetch the model checkpoint weights and the reference speaker voice files:

```powershell
# 1. Download the VibeVoice-Realtime-0.5B model checkpoint from Hugging Face (~1.1 GB)
.venv-vibevoice\Scripts\python.exe scripts/vibevoice/download_model.py

# 2. Download the default reference speaker voice files (.pt prompt cache files) from GitHub (~95 MB)
.venv-vibevoice\Scripts\python.exe scripts/vibevoice/download_voices.py
```

## Running the Server

Start the TTS server in offline mode to prevent Hugging Face Hub from triggering network handshake requests:

```powershell
# 1. Enable Hugging Face offline mode
$env:HF_HUB_OFFLINE=1

# 2. Start the uvicorn server on CUDA GPU
.venv-vibevoice\Scripts\python.exe scripts/vibevoice/server.py --device cuda --model-path models/vibevoice
```
*The server will initialize the CUDA context, load the model, and bind to port `8091`.*

---

## Configuration with voice-pipes

Point the `openai` TTS provider at the local VibeVoice server in your pipeline config (e.g. `examples/live/moonshine.toml`):

```toml
[tts]
provider = "openai"
endpoint = "http://localhost:8091/v1"
model_name = "VibeVoice-Realtime-0.5B"
voice = "default"
api_key = "local"
```

Because the voice-pipes OpenAI TTS provider includes automatic WAV header detection and stripping, it will consume the 24kHz audio stream directly as raw PCM samples.

---

## Troubleshooting & Optimization Notes

### 1. `ValueError: Already used by a Transformers model`
* **Cause**: Hugging Face's dynamic remote code loader (`trust_remote_code=True`) imports files dynamically. When the package is also imported by `server.py`, the tokenizer and model configuration classes get registered twice.
* **Fix**: Wrapped all `AutoModel.register()` and `AutoModelForCausalLM.register()` calls in `vendor/vibevoice/vibevoice/modular/*.py` in a `try...except ValueError: pass` block to permit clean reloads.

### 2. `RuntimeError: Expected query, key, and value to have the same dtype, but got Half and float`
* **Cause**: The prefilled prompt KV caches (`.pt` files in `demo/voices`) are saved in `float32`. Running the model in `float16` or `bfloat16` on CUDA causes a datatype mismatch in the attention layers.
* **Fix**: Implemented a recursive `cast_tensors` helper in `server.py` that crawls loaded voice dictionary/list/tuple/Cache hierarchies in-place, casting all floating-point tensors to the model's active dtype (`float16`).

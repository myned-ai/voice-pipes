#!/usr/bin/env bash
set -euo pipefail

MODELS_DIR="${MODELS_DIR:-/app/models}"
LLAMA_MODEL_FILE="${LLAMA_MODEL_FILE:-qwen2.5-1.5b-instruct-q4_k_m.gguf}"
LLAMA_MODEL_URL="${LLAMA_MODEL_URL:-https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf}"

mkdir -p "${MODELS_DIR}/moonshine"

download_if_missing() {
    local url="$1"
    local dest="$2"
    if [ -s "${dest}" ]; then
        echo "[models] cached: ${dest}"
        return 0
    fi
    echo "[models] downloading: ${dest}"
    local tmp="${dest}.part"
    if [ -n "${HF_TOKEN:-}" ]; then
        curl -fL --retry 5 --retry-delay 3 -H "Authorization: Bearer ${HF_TOKEN}" "${url}" -o "${tmp}"
    else
        curl -fL --retry 5 --retry-delay 3 "${url}" -o "${tmp}"
    fi
    mv -f "${tmp}" "${dest}"
}

# 1. Silero VAD (~2 MB)
download_if_missing \
    "https://github.com/snakers4/silero-vad/raw/master/src/silero_vad/data/silero_vad.onnx" \
    "${MODELS_DIR}/silero_vad.onnx"

# 2. Moonshine Base ONNX STT (~240 MB)
download_if_missing \
    "https://huggingface.co/onnx-community/moonshine-base-ONNX/resolve/main/onnx/encoder_model.onnx" \
    "${MODELS_DIR}/moonshine/encoder_model.onnx"
download_if_missing \
    "https://huggingface.co/onnx-community/moonshine-base-ONNX/resolve/main/onnx/decoder_model_merged.onnx" \
    "${MODELS_DIR}/moonshine/decoder_model_merged.onnx"
download_if_missing \
    "https://huggingface.co/onnx-community/moonshine-base-ONNX/resolve/main/tokenizer.json" \
    "${MODELS_DIR}/moonshine/tokenizer.json"

# 3. CPU LLM GGUF (Qwen 2.5 1.5B Instruct Q4_K_M, ~1.1 GB, Apache 2.0, ungated)
#    (Pocket-TTS weights are fetched and cached automatically by pocket-tts into HF_HOME)
download_if_missing \
    "${LLAMA_MODEL_URL}" \
    "${MODELS_DIR}/${LLAMA_MODEL_FILE}"

echo "[models] all CPU pipeline models ready in ${MODELS_DIR}"

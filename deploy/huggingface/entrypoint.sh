#!/usr/bin/env bash
set -euo pipefail

# Persist models in /data/models if Hugging Face Persistent Storage is attached,
# otherwise keep them in /app/models inside the ephemeral container filesystem.
if [ -d "/data" ] && [ -w "/data" ]; then
    export MODELS_DIR="${MODELS_DIR:-/data/models}"
else
    export MODELS_DIR="${MODELS_DIR:-/app/models}"
fi

mkdir -p "${MODELS_DIR}" /app/data/captures /app/data/audit /app/data/sessions /app/data/state

# Symlink /app/models -> $MODELS_DIR when using /data/models so TOML paths stay fixed
if [ "${MODELS_DIR}" != "/app/models" ] && [ ! -L "/app/models" ]; then
    rm -rf /app/models
    ln -s "${MODELS_DIR}" /app/models
fi

# Generate strong internal tokens at boot if not provided via Space Secrets.
# These stay strictly inside localhost (orchestrator <-> worker <-> BFF).
gen_secret() {
    python3 -c "import secrets; print(secrets.token_hex(32))"
}

export VP_ADMIN_TOKEN="${VP_ADMIN_TOKEN:-$(gen_secret)}"
export VP_INTERNAL_TOKEN="${VP_INTERNAL_TOKEN:-$(gen_secret)}"
export VP_CLIENT_TOKEN="${VP_CLIENT_TOKEN:-$(gen_secret)}"
export ADMIN_SESSION_SECRET="${ADMIN_SESSION_SECRET:-$(gen_secret)}"

# Multi-tenant, feature flags & automatic disk cleanup defaults for Hugging Face Spaces
export ADMIN_MULTI_TENANT="${ADMIN_MULTI_TENANT:-true}"
export ADMIN_UI_AUTH_REQUIRED="false"
export ADMIN_FEATURE_BENCHMARKS="${ADMIN_FEATURE_BENCHMARKS:-false}"
export ADMIN_FEATURE_TEST_SUITES="${ADMIN_FEATURE_TEST_SUITES:-false}"
export ADMIN_CAPTURE_TTL_MINUTES="${ADMIN_CAPTURE_TTL_MINUTES:-30}"
export ADMIN_CAPTURES_MAX_MB="${ADMIN_CAPTURES_MAX_MB:-500}"
export ADMIN_CLEANUP_INTERVAL_SECONDS="${ADMIN_CLEANUP_INTERVAL_SECONDS:-300}"
export ADMIN_STATE_ROOT="/app/data/state"
export ORCHESTRATOR_URL="http://127.0.0.1:9090"
export CAPTURES_DIR="/app/data/captures"

# Admission control, FIFO waiting queue & session duration cap defaults for HF Space demo
export VP_ADMISSION_ENABLED="${VP_ADMISSION_ENABLED:-true}"
export VP_ADMISSION_MAX_ACTIVE="${VP_ADMISSION_MAX_ACTIVE:-3}"
export VP_ADMISSION_MAX_PER_TENANT="${VP_ADMISSION_MAX_PER_TENANT:-1}"
export VP_ADMISSION_MAX_QUEUE="${VP_ADMISSION_MAX_QUEUE:-15}"
export VP_ADMISSION_QUEUE_TIMEOUT_S="${VP_ADMISSION_QUEUE_TIMEOUT_S:-180}"
export VP_MAX_SESSION_DURATION_S="${VP_MAX_SESSION_DURATION_S:-300}"

# CORS: allow the Space's own public host + huggingface.co iframe origin
if [ -z "${ADMIN_CORS_ORIGINS:-}" ]; then
    if [ -n "${SPACE_HOST:-}" ]; then
        export ADMIN_CORS_ORIGINS="https://${SPACE_HOST},https://huggingface.co"
    else
        export ADMIN_CORS_ORIGINS="http://localhost:7860,http://127.0.0.1:7860"
    fi
fi

# Cache Hugging Face Hub downloads (including Pocket-TTS weights) inside MODELS_DIR
# so attaching HF Persistent Storage (/data) avoids re-downloading across restarts.
export HF_HOME="${MODELS_DIR}/.cache/huggingface"
mkdir -p "${HF_HOME}"

# Auto-tune CPU thread pools based on available vCPUs
CPUS="$(nproc 2>/dev/null || echo 4)"
export VP_ORT_EP="cpu"
export VP_ORT_THREADS="${VP_ORT_THREADS:-2}"
export OMP_NUM_THREADS="1"
export MKL_NUM_THREADS="1"

export LLAMA_MODEL_FILE="${LLAMA_MODEL_FILE:-qwen2.5-1.5b-instruct-q4_k_m.gguf}"
export LLAMA_THREADS="${LLAMA_THREADS:-$(( CPUS > 2 ? CPUS - 2 : CPUS ))}"
export LLAMA_PARALLEL="${LLAMA_PARALLEL:-4}"
export LLAMA_CTX="${LLAMA_CTX:-2048}"

echo "[entrypoint] starting Voice Pipes CPU Space (vCPUs=${CPUS}, llama_threads=${LLAMA_THREADS}, ttl=${ADMIN_CAPTURE_TTL_MINUTES}m, cap=${ADMIN_CAPTURES_MAX_MB}MB)"

# Download any missing models before launching services
/app/hf/download-models.sh

exec supervisord -c /app/hf/supervisord.conf

#!/bin/bash
# Unified Docker-based fleet configurator & launcher for Voice Pipes (Linux/macOS).
#
# --take-ports: also stop FOREIGN containers (other compose projects) that hold
#               ports this stack needs; without it they are named and the launch
#               aborts with instructions.
# --skip-wizard: start the stack on the configuration already under
#               voice-pipes/deploy/docker/config/ (from an earlier run, or from
#               `voice-pipes config-wizard` run natively) instead of asking again.

set -e

TAKE_PORTS=0
SKIP_WIZARD=0
FORCE_CPU=0
for arg in "$@"; do
    case "$arg" in
        --take-ports) TAKE_PORTS=1 ;;
        --skip-wizard) SKIP_WIZARD=1 ;;
        --cpu|--no-gpu) FORCE_CPU=1 ;;
        *) echo "Unknown option: $arg (supported: --take-ports, --skip-wizard, --cpu, --no-gpu)"; exit 2 ;;
    esac
done

# Shared port-conflict preflight (stops our own stale containers; names
# foreign holders instead of failing with a bare "port is already allocated").
# shellcheck source=scripts/preflight-ports.sh
. "$(cd "$(dirname "$0")" && pwd)/scripts/preflight-ports.sh"

echo "======================================================="
echo "      VOICE PIPES FLEET LAUNCHER (DOCKER)              "
echo "======================================================="

# 1. Verify Docker is running
echo "🔍 Checking if Docker is running..."
if ! docker info >/dev/null 2>&1; then
    echo "❌ Error: Docker is not running or not accessible. Please start Docker and try again."
    exit 1
fi
echo "✅ Docker is active."

# 2. Probe GPU Support
if [ "$FORCE_CPU" -eq 1 ] || [ "$VP_DEVICE" = "cpu" ] || [ -n "$VP_FORCE_CPU" ]; then
    gpu_supported=false
    echo "ℹ️  Running in CPU-only mode (--cpu flag / VP_DEVICE=cpu)."
else
    echo "🔍 Probing for NVIDIA GPU container pass-through capability..."
    gpu_supported=false
    if docker run --rm --gpus all nvidia/cuda:12.4.1-base-ubuntu22.04 nvidia-smi >/dev/null 2>&1; then
        gpu_supported=true
        echo "⚡ NVIDIA GPU container support detected!"

        compute_cap=$(docker run --rm --gpus all nvidia/cuda:12.4.1-base-ubuntu22.04 nvidia-smi --query-gpu=compute_cap --format=csv,noheader 2>/dev/null | tr -d '[:space:]' || true)
        if [ -n "$compute_cap" ]; then
            major=$(echo "$compute_cap" | cut -d'.' -f1)
            if [ -n "$major" ] && [ "$major" -ge 12 ] 2>/dev/null; then
                if [ -z "$VP_ORT_EP" ]; then
                    export VP_ORT_EP="cpu"
                    echo "⚠️  Detected NVIDIA Blackwell GPU (compute capability $compute_cap)."
                    echo "   Setting VP_ORT_EP=cpu for ONNX models (llama-server continues on GPU)."
                fi
            fi
        fi
    else
        echo "ℹ️ NVIDIA GPU pass-through is not available on this Docker engine."
        echo "   Defaulting to CPU-only execution."
    fi
fi

if [ "$SKIP_WIZARD" = 1 ]; then
    if [ ! -f voice-pipes/deploy/docker/config/pipeline-custom.toml ]; then
        echo "❌ Error: --skip-wizard needs an existing configuration (voice-pipes/deploy/docker/config/pipeline-custom.toml); run without it first."
        exit 1
    fi
    echo "⏭️  Skipping the wizard; using the existing configuration."
else
# 3. Build the builder container for the config wizard
# Build context is the repo root so the `voice-pipes` crate can see its
# `../voice-pipes-sdk` path dependency.
echo "🔨 Preparing configuration wizard container..."
docker build -t voice-pipes-builder -f voice-pipes/Dockerfile .

# 3b. Resolve a HuggingFace token (needed only for gated repos, e.g. Gemma 3).
# The wizard downloads models inside the container, so the token must be passed
# in via HF_TOKEN. Public models (SmolLM2, Parakeet, Piper, Moonshine) work
# without one.
echo "🔍 Checking for HuggingFace access token (needed for gated models like Gemma 3)..."
if [ -z "${HF_TOKEN:-}" ] && [ -f "$HOME/.cache/huggingface/token" ]; then
    HF_TOKEN=$(tr -d '[:space:]' < "$HOME/.cache/huggingface/token")
    export HF_TOKEN
    echo "🔑 Using HuggingFace token from ~/.cache/huggingface/token"
fi
if [ -z "${HF_TOKEN:-}" ]; then
    echo "ℹ️ No HF_TOKEN set. Public models download fine; gated models (e.g. Gemma 3) need one."
    echo "   Create a read token at https://huggingface.co/settings/tokens"
    read -rsp "Paste HuggingFace token (or press Enter to skip): " HF_TOKEN || true
    echo
    export HF_TOKEN
fi
hf_env=()
if [ -n "${HF_TOKEN:-}" ]; then hf_env=(-e HF_TOKEN); fi

# 4. Run the interactive configurator wizard
echo "🚀 Launching interactive configuration wizard..."
# VP_WIZARD_FROM_LAUNCHER tells the wizard this script starts the stack
# next, so it prints no next step of its own.
docker run -it --rm "${hf_env[@]}" -e VP_WIZARD_FROM_LAUNCHER=1 -v "$(pwd)/voice-pipes:/app" voice-pipes-builder config-wizard
fi

# 5. Start the backend services (Orchestrator + Worker)
echo "🚀 Starting Voice Pipes backend fleet..."

# The wizard hands off its sidecar decisions in one declarative manifest
# (stack-manifest.env): COMPOSE_PROFILES, SIDECAR_SERVICES, REQUIRES_GPU,
# ENSURE_DIRS, plus arbitrary env vars (e.g. LLAMA_MODEL) for docker-compose
# interpolation. This section is fully generic — adding a new sidecar to the
# wizard requires NO launcher changes. The file is quoted KEY="VALUE" lines,
# so it can be sourced directly.
MANIFEST="voice-pipes/deploy/docker/config/stack-manifest.env"
COMPOSE_PROFILES=""
SIDECAR_SERVICES=""
REQUIRES_GPU=""
ENSURE_DIRS=""
if [ -f "$MANIFEST" ]; then
    set -a
    # shellcheck disable=SC1090
    . "$MANIFEST"
    set +a
else
    echo "ℹ️ No stack manifest found ($MANIFEST); starting core services only."
    echo "   Re-run the wizard (or this script from the top) to configure sidecars."
fi

# Load API keys the wizard stored in voice-pipes/.env (the wizard runs inside
# the builder container with /app = voice-pipes/, so the file lands next to
# the crate — not in the compose project dir, where compose would auto-read
# it). The compose file passes these through by name (OPENAI_API_KEY,
# GEMINI_API_KEY, ELEVENLABS_API_KEY for the ElevenLabs Agents S2S worker,
# ASSEMBLYAI_API_KEY, ...), so they must exist in this process environment.
# Values already set in the shell win, so a deliberate override survives.
DOTENV="voice-pipes/.env"
# The shipped deploy configs set `[auth] required = true`, so the
# orchestrator and workers refuse to start without these three tokens.
# Generate each missing one once and keep it in voice-pipes/.env; the loop
# below exports them and compose passes them through by name.
for tok in VP_CLIENT_TOKEN VP_ADMIN_TOKEN VP_INTERNAL_TOKEN; do
    if ! grep -qs "^${tok}=..*" "$DOTENV" && [ -z "$(printenv "$tok" 2>/dev/null)" ]; then
        new_token=$(openssl rand -hex 32 2>/dev/null || head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
        printf '%s=%s\n' "$tok" "$new_token" >> "$DOTENV"
        echo "🔐 Generated $tok into $DOTENV"
    fi
done
if [ -f "$DOTENV" ]; then
    while IFS= read -r line; do
        case "$line" in ''|\#*) continue ;; esac
        key=${line%%=*}
        value=${line#*=}
        # Accept KEY="value" and KEY='value' the way the PowerShell
        # launcher does, so a pasted key works on both platforms.
        case "$value" in
            \"*\") value=${value#\"}; value=${value%\"} ;;
            \'*\') value=${value#\'}; value=${value%\'} ;;
        esac
        if [ -n "$key" ] && ! printenv "$key" >/dev/null 2>&1; then
            export "$key=$value"
        fi
    done < "$DOTENV"
    echo "🔑 Loaded stored API keys from $DOTENV (shell-set values take precedence)."
fi

# Warn when a configured sidecar needs GPU pass-through we didn't detect.
if [ -n "$REQUIRES_GPU" ] && [ "$gpu_supported" != true ]; then
    echo "⚠️  Warning: $REQUIRES_GPU require(s) GPU pass-through, which wasn't detected."
    echo "   The service(s) will start but may be unusably slow on CPU. Enable the"
    echo "   NVIDIA Container Toolkit / WSL GPU support for usable latency."
fi

# Pre-create bind-mount source dirs (relative to voice-pipes/) so Docker
# doesn't create them as root-owned paths.
for dir in $ENSURE_DIRS; do
    mkdir -p "voice-pipes/$dir"
done

# Containers run unprivileged. On Linux the bind-mounted data dirs
# belong to the invoking user, so run the containers as that user; compose
# reads VP_UID/VP_GID (`user: "${VP_UID:-10001}:${VP_GID:-10001}"`). Docker
# Desktop (macOS/Windows) ignores ownership, where the default uid is fine.
if [ -z "${VP_UID:-}" ] && command -v id >/dev/null 2>&1 && [ "$(id -u)" != "0" ]; then
    export VP_UID="$(id -u)" VP_GID="$(id -g)"
fi

# Launcher policy (host-side knowledge, not the wizard's): GPU offload level
# for the llama-server sidecar. Override by setting LLAMA_GPU_LAYERS yourself.
case " $SIDECAR_SERVICES " in
    *" llama-server "*)
        if [ -z "${LLAMA_GPU_LAYERS:-}" ]; then
            if [ "$gpu_supported" = true ]; then export LLAMA_GPU_LAYERS=99; else export LLAMA_GPU_LAYERS=0; fi
            echo "🦙 llama-server GPU layers: $LLAMA_GPU_LAYERS (override with LLAMA_GPU_LAYERS)"
        fi
        ;;
esac

# Decide which worker to start; sidecars must be named explicitly in `up`
# (an active profile alone does not pull them into an `up` with named services).
if [ "$gpu_supported" = true ]; then
    WORKER="worker-custom-gpu"
    COMPOSE_PROFILES="${COMPOSE_PROFILES:+$COMPOSE_PROFILES,}gpu"
else
    WORKER="worker-custom"
fi
export COMPOSE_PROFILES

if [ -n "$SIDECAR_SERVICES" ]; then
    echo "🧩 Sidecars from manifest: $SIDECAR_SERVICES"
fi
echo "Starting: orchestrator $WORKER $SIDECAR_SERVICES"

# Port preflight: auto-stops stale containers from this stack (including the
# OTHER worker variant when switching CPU<->GPU) and names foreign holders.
# shellcheck disable=SC2086 # SIDECAR_SERVICES is a space-separated list
if ! resolve_port_conflicts "voice-pipes/deploy/docker/docker-compose.yml" "$TAKE_PORTS" orchestrator "$WORKER" $SIDECAR_SERVICES; then
    echo "❌ Error: Port conflicts unresolved — see messages above (or re-run with --take-ports)."
    exit 1
fi

# shellcheck disable=SC2086 # SIDECAR_SERVICES is a space-separated list
docker compose -f voice-pipes/deploy/docker/docker-compose.yml up --build -d orchestrator "$WORKER" $SIDECAR_SERVICES

# 6. Start the Admin UI
# The admin backend refuses to start without ADMIN_UI_TOKEN. Compose
# reads voice-pipes-admin/.env (its project dir) for interpolation, so keep
# the token there; generate one on first run and reuse it afterwards.
ADMIN_ENV="voice-pipes-admin/.env"
if ! grep -qs '^ADMIN_UI_TOKEN=..*' "$ADMIN_ENV"; then
    new_token=$(openssl rand -hex 32 2>/dev/null || head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
    printf 'ADMIN_UI_TOKEN=%s\n' "$new_token" >> "$ADMIN_ENV"
    echo "🔐 Generated ADMIN_UI_TOKEN into $ADMIN_ENV (the console asks for it on first load)."
fi
ADMIN_UI_TOKEN=$(grep '^ADMIN_UI_TOKEN=' "$ADMIN_ENV" | tail -n 1 | cut -d= -f2-)
export ADMIN_UI_TOKEN
echo "🚀 Starting Admin UI..."
if ! resolve_port_conflicts "voice-pipes-admin/docker-compose.yml" "$TAKE_PORTS" backend frontend; then
    echo "❌ Error: Port conflicts unresolved — see messages above (or re-run with --take-ports)."
    exit 1
fi
docker compose -f voice-pipes-admin/docker-compose.yml up --build -d

echo ""
echo "🎉 Voice Pipes Fleet started successfully!"
echo "👉 Admin UI available at: http://localhost:3000"
echo "   Sign-in token: ADMIN_UI_TOKEN in $ADMIN_ENV (not printed; it would stay in this terminal's scrollback)"
echo "👉 Orchestrator API at:   http://localhost:9090"
echo "👉 Worker WebSocket at:   ws://localhost:8800/ws"
echo ""

# 7. Follow logs
echo "📺 Streaming logs from Custom Worker & Orchestrator (Press Ctrl+C to stop)..."
docker compose -f voice-pipes/deploy/docker/docker-compose.yml logs -f

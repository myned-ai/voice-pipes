#!/bin/bash
# Unified Docker-based fleet configurator & launcher for Voice Pipes (Linux/macOS).
#
# --take-ports: also stop FOREIGN containers (other compose projects) that hold
#               ports this stack needs; without it they are named and the launch
#               aborts with instructions.
# --skip-wizard: start the stack on the configuration already under
#               voice-pipes/deploy/docker/config/ (from an earlier run, or from
#               `voice-pipes config-wizard` run natively) instead of asking again.
# --full: compile all providers including llama.cpp-backed ones (in-process
#         GGUF LLMs like SmolLM2, and NeuTTS-Air). Omit for much faster builds
#         using ONNX, cloud, and sidecar (llama-server) providers.

set -e

print_help() {
    cat <<'EOF'
Usage: ./launch-stack.sh [OPTIONS]

Unified Docker-based fleet configurator & launcher for Voice Pipes (Linux/macOS).

Options:
  --take-ports    Also stop FOREIGN containers (other compose projects) holding
                  ports this stack needs; without it, foreign holders are named
                  and the launch aborts.
  --skip-wizard   Start the stack using the existing configuration under
                  voice-pipes/deploy/docker/config/ instead of running the wizard.
  --cpu, --no-gpu Force CPU-only execution (disables GPU pass-through probing).
  --build         Compile images locally from source instead of pulling
                  prebuilt images from GHCR.
  --rebuild       Force recompiling the voice-pipes-builder wizard container
                  (implies --build).
  --full          Include all providers when building locally, including
                  llama.cpp-backed ones (in-process GGUF LLMs like SmolLM2,
                  and NeuTTS-Air). Prebuilt GHCR images already include all
                  providers.
  -h, --help, -?  Print this help message and exit.

Examples:
  ./launch-stack.sh                     # Pull prebuilt images + interactive wizard
  ./launch-stack.sh --build             # Fast local compile + interactive wizard
  ./launch-stack.sh --build --full      # Full local compile including SmolLM2 GGUF
  ./launch-stack.sh --skip-wizard       # Reuse existing config
EOF
}

TAKE_PORTS=0
SKIP_WIZARD=0
FORCE_CPU=0
BUILD_LOCAL=0
REBUILD=0
FULL_BUILD=0
for arg in "$@"; do
    case "$arg" in
        --take-ports) TAKE_PORTS=1 ;;
        --skip-wizard) SKIP_WIZARD=1 ;;
        --cpu|--no-gpu) FORCE_CPU=1 ;;
        --build) BUILD_LOCAL=1 ;;
        --rebuild) REBUILD=1; BUILD_LOCAL=1 ;;
        --full) FULL_BUILD=1 ;;
        -h|--help|-\?) print_help; exit 0 ;;
        *) echo "Unknown option: $arg (run with --help for usage)"; exit 2 ;;
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

LITE_FEATURES="realtime,stt-parakeet,tts-piper,vad-silero,filter-deepfilter,llm-openai-compat,s2s-gemini,s2s-openai,s2s-elevenlabs,telemetry,wizard,audio-device,session-sqlite,text-grpc,tts-kokoro,tts-pocket,stt-moonshine,tts-openai,tts-elevenlabs,stt-assemblyai,stt-google,tts-gemini,stt-nemotron,stt-nemotron35,turn-smart"
CFG_PATH="voice-pipes/deploy/docker/config/pipeline-custom.toml"
if [ "$FULL_BUILD" -eq 0 ] && [ "$SKIP_WIZARD" -eq 1 ] && [ -f "$CFG_PATH" ] && grep -Eq 'provider\s*=\s*["'"'"'](neutts|gguf)["'"'"']' "$CFG_PATH"; then
    FULL_BUILD=1
    echo "ℹ️  Detected neutts/gguf in $CFG_PATH; enabling --full build features."
fi

builder_build_args=()
compose_build_args=()
if [ "$FULL_BUILD" -eq 1 ]; then
    echo "🔨 Build profile: full (includes in-process GGUF LLMs)."
else
    echo "⚡ Build profile: fast (skipping optional llama.cpp C++/CUDA build)."
    echo "   (Pass --full if you plan to use in-process GGUF LLMs like SmolLM2 or NeuTTS-Air)"
    builder_build_args=(--build-arg "CARGO_FLAGS=--no-default-features" --build-arg "FEATURES=$LITE_FEATURES")
    compose_build_args=(--build-arg "CARGO_FLAGS=--no-default-features" --build-arg "FEATURES=$LITE_FEATURES" --build-arg "GPU_FEATURES=ort-cuda")
fi

if [ "$SKIP_WIZARD" = 1 ]; then
    if [ ! -f "$CFG_PATH" ]; then
        echo "❌ Error: --skip-wizard needs an existing configuration ($CFG_PATH); run without it first."
        exit 1
    fi
    echo "⏭️  Skipping the wizard; using the existing configuration."
else
# 3. Prepare wizard container (pull prebuilt image by default, or build locally with --build/--rebuild)
WIZARD_IMAGE=""
PREBUILT_IMAGE="${VP_IMAGE:-ghcr.io/myned-ai/voice-pipes:latest}"
if [ "$BUILD_LOCAL" -eq 0 ]; then
    echo "📥 Pulling prebuilt wizard image ($PREBUILT_IMAGE)..."
    if docker pull "$PREBUILT_IMAGE"; then
        WIZARD_IMAGE="$PREBUILT_IMAGE"
    else
        echo "⚠️  Could not pull $PREBUILT_IMAGE; falling back to local build (--build)."
        BUILD_LOCAL=1
    fi
fi

if [ -z "$WIZARD_IMAGE" ]; then
    builder_id=$(docker images -q voice-pipes-builder 2>/dev/null | head -n 1)
    if [ -n "$builder_id" ] && [ "$REBUILD" -ne 1 ]; then
        echo "✅ Using existing voice-pipes-builder image (${builder_id:0:12})."
        echo "   (Pass --rebuild to recompile the wizard container)"
    else
        echo "🔨 Preparing configuration wizard container..."
        docker build "${builder_build_args[@]}" -t voice-pipes-builder -f voice-pipes/Dockerfile .
    fi
    WIZARD_IMAGE="voice-pipes-builder"
fi

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
docker run -it --rm "${hf_env[@]}" -e VP_WIZARD_FROM_LAUNCHER=1 -v "$(pwd)/voice-pipes:/app" "$WIZARD_IMAGE" config-wizard
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

# Pre-create bind-mount source dirs so Docker doesn't create them as
# root-owned paths.
for dir in $ENSURE_DIRS; do
    if [ "${dir#vendor/}" != "$dir" ]; then
        mkdir -p "$dir"
    else
        mkdir -p "voice-pipes/$dir"
    fi
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

# Re-check pipeline-custom.toml after wizard in case the user picked neutts/gguf
if [ "$FULL_BUILD" -eq 0 ] && [ -f "$CFG_PATH" ] && grep -Eq 'provider\s*=\s*["'"'"'](neutts|gguf)["'"'"']' "$CFG_PATH"; then
    FULL_BUILD=1
    compose_build_args=()
    echo "ℹ️  Detected neutts/gguf in $CFG_PATH; enabling llama.cpp build features for worker."
fi

# Pull prebuilt images by default, or build locally when --build is set
if [ "$BUILD_LOCAL" -eq 0 ]; then
    echo "📥 Pulling prebuilt backend fleet container images..."
    if ! docker compose -f voice-pipes/deploy/docker/docker-compose.yml pull orchestrator "$WORKER"; then
        echo "⚠️  Pull failed; falling back to local build (--build)."
        BUILD_LOCAL=1
    fi
fi
if [ "$BUILD_LOCAL" -eq 1 ]; then
    # Build images sequentially to prevent apt mirror rate-limiting and port-80 connection collisions
    echo "🔨 Preparing backend fleet container images..."
    docker compose -f voice-pipes/deploy/docker/docker-compose.yml build "${compose_build_args[@]}" orchestrator
    # shellcheck disable=SC2086
    docker compose -f voice-pipes/deploy/docker/docker-compose.yml build "${compose_build_args[@]}" "$WORKER"
fi

# shellcheck disable=SC2086 # SIDECAR_SERVICES is a space-separated list
docker compose -f voice-pipes/deploy/docker/docker-compose.yml up -d --remove-orphans orchestrator "$WORKER" $SIDECAR_SERVICES

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
if [ "$BUILD_LOCAL" -eq 0 ]; then
    if ! docker compose -f voice-pipes-admin/docker-compose.yml pull; then
        echo "⚠️  Admin UI pull failed; falling back to local build."
        docker compose -f voice-pipes-admin/docker-compose.yml up --build -d
    else
        docker compose -f voice-pipes-admin/docker-compose.yml up -d
    fi
else
    docker compose -f voice-pipes-admin/docker-compose.yml up --build -d
fi

# 6.5 Verify Worker & Fleet Readiness
echo "⏳ Verifying backend fleet readiness..."
worker_id=$(docker compose -f voice-pipes/deploy/docker/docker-compose.yml ps -q "$WORKER" 2>/dev/null)
if [ -n "$worker_id" ]; then
    cfg_path="voice-pipes/deploy/docker/config/pipeline-custom.toml"
    is_heavy=false
    if [ -f "$cfg_path" ] && grep -E 'provider\s*=\s*["'"'"']neutts["'"'"']' "$cfg_path" >/dev/null 2>&1; then
        is_heavy=true
        echo "   ℹ️  NeuTTS-Air detected: loading neural speech model & vocoder on CPU."
        echo "   ℹ️  Initial model allocation may take 1-3 minutes. Please wait..."
    fi

    worker_timeout=60
    if [ "$is_heavy" = true ]; then
        worker_timeout=240
    fi

    echo "Waiting for worker '$WORKER' readiness..."
    worker_ready=false
    elapsed=0
    while [ $elapsed -lt $worker_timeout ]; do
        sleep 2
        elapsed=$((elapsed + 2))
        status=$(docker inspect --format '{{.State.Status}}' "$worker_id" 2>/dev/null)
        if [ "$status" = "restarting" ] || [ "$status" = "exited" ]; then
            echo ""
            echo "❌ Worker failed startup validation (status: $status)!"
            docker logs --tail 25 "$worker_id" 2>&1 | grep -E '(ERROR voice_pipes|config validation failed|model load failed|missing model file)' || true
            echo ""
            echo "💡 To view full diagnostic logs: docker compose -f voice-pipes/deploy/docker/docker-compose.yml logs $WORKER"
            exit 1
        fi

        h_status=$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}unknown{{end}}' "$worker_id" 2>/dev/null)
        if [ "$h_status" = "healthy" ]; then
            worker_ready=true
            echo "✅ Worker '$WORKER' is healthy and registered with orchestrator."
            break
        fi

        if [ $((elapsed % 10)) -eq 0 ]; then
            echo "   ... still warming up models (${elapsed}s elapsed)"
        fi
    done

    if [ "$worker_ready" = false ]; then
        echo "⚠️  Note: Worker '$WORKER' is still initializing in the background. It will become active as soon as models finish loading."
    fi
fi

# Wait for sidecar healthchecks (e.g. pocket-tts, llama-server) before opening fleet
for sidecar in $SIDECAR_SERVICES; do
    [ -z "$sidecar" ] && continue
    sidecar_id=$(docker compose -f voice-pipes/deploy/docker/docker-compose.yml ps -q "$sidecar" 2>/dev/null)
    if [ -n "$sidecar_id" ]; then
        has_health=$(docker inspect --format '{{if .State.Health}}true{{else}}false{{end}}' "$sidecar_id" 2>/dev/null)
        if [ "$has_health" = "true" ]; then
            echo "Waiting for sidecar '$sidecar' readiness..."
            ready=false
            for s in $(seq 1 90); do
                h_status=$(docker inspect --format '{{.State.Health.Status}}' "$sidecar_id" 2>/dev/null)
                c_status=$(docker inspect --format '{{.State.Status}}' "$sidecar_id" 2>/dev/null)
                if [ "$c_status" = "exited" ] || [ "$c_status" = "restarting" ]; then
                    echo "❌ Sidecar '$sidecar' failed to start (status: $c_status)!"
                    docker compose -f voice-pipes/deploy/docker/docker-compose.yml logs --tail 30 "$sidecar"
                    exit 1
                fi
                if [ "$h_status" = "healthy" ]; then
                    ready=true
                    echo "✓ Sidecar '$sidecar' is healthy and ready."
                    if [ "$sidecar" = "llama-server" ]; then
                        curl -sf -X POST http://127.0.0.1:8081/v1/chat/completions \
                            -H "Content-Type: application/json" \
                            -d '{"model":"default","messages":[{"role":"user","content":"hi"}],"max_tokens":1}' >/dev/null 2>&1 || true
                        echo "  [+] llama-server CUDA graphs & KV cache warmed."
                    fi
                    break
                fi
                sleep 2
            done
            if [ "$ready" = false ]; then
                echo "⚠️ Warning: Sidecar '$sidecar' did not report healthy within timeout."
            fi
        fi
    fi
done

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

# All-in-one CPU-only Hugging Face Spaces Docker image for Voice Pipes:
#   - Voice Pipes Orchestrator + Worker (Silero VAD + Moonshine STT + Pocket-TTS HTTP on CPU)
#   - llama-server (Qwen 2.5 1.5B Instruct Q4_K_M on CPU)
#   - pocket-tts HTTP server (Kyutai Pocket-TTS on CPU, port 8002)
#   - Admin Console Backend (FastAPI, multi-tenant HF OAuth / Guest + TTL capture janitor)
#   - Admin Console Frontend (React 18 + Vite static bundle)
#   - Nginx reverse proxy on port 7860 (Hugging Face Spaces entrypoint)
#
# Build from the repository root:
#   docker build -f deploy/huggingface/Dockerfile -t voice-pipes-hf-cpu .

# ── Stage 1: Build lean CPU-only Voice Pipes Rust binary ─────
FROM rust:1.91-trixie AS rust-builder
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        ca-certificates \
        cmake \
        clang \
        libclang-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
ENV CARGO_NET_RETRY=10 \
    CARGO_HTTP_TIMEOUT=120 \
    GGML_NATIVE=OFF

ARG HF_FEATURES="stt-moonshine,tts-pocket,vad-silero,llm-openai-compat,session-sqlite,telemetry"

COPY Cargo.toml Cargo.lock ./
COPY voice-pipes ./voice-pipes
COPY voice-pipes-sdk ./voice-pipes-sdk
COPY voice-pipes-bench ./voice-pipes-bench
COPY voice-pipes-gym ./voice-pipes-gym
COPY voice-pipes-cli ./voice-pipes-cli
COPY voice-pipes-onnx ./voice-pipes-onnx
COPY voice-pipes-net ./voice-pipes-net
COPY plugins ./plugins

RUN cargo build --release -p voice-pipes-cli --no-default-features --features "${HF_FEATURES}" \
    && cp /app/target/release/voice-pipes /usr/local/bin/voice-pipes

# ── Stage 2: Prebuilt CPU llama-server ───────────────────────
FROM ghcr.io/ggml-org/llama.cpp:server-b9994 AS llama-bin

# ── Stage 3: Build Admin UI (same-origin relative API URL) ───
FROM node:22-alpine AS frontend-builder
WORKDIR /frontend
COPY voice-pipes-admin/frontend/package.json voice-pipes-admin/frontend/package-lock.json ./
RUN npm ci
COPY voice-pipes-admin/frontend/ ./
# Empty VITE_API_URL makes the bundle use same-origin /api/v1/* paths behind Nginx :7860
ARG VITE_API_URL=""
ARG VITE_WS_URL=""
RUN npm run build

# ── Stage 4: Runtime container (HF Spaces UID 1000, port 7860) ─
FROM python:3.12-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        nginx \
        supervisor \
        libgomp1 \
        curl \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Hugging Face Spaces requires running as non-root UID 1000
RUN useradd -m -u 1000 user

WORKDIR /app

# Python Admin BFF dependencies (hash-pinned)
COPY voice-pipes-admin/backend/requirements.txt /app/backend/requirements.txt
RUN pip install --no-cache-dir --require-hashes -r /app/backend/requirements.txt

# Isolated virtualenv for Kyutai Pocket-TTS HTTP server (CPU PyTorch)
RUN python3 -m venv /opt/pocket-tts \
    && /opt/pocket-tts/bin/pip install --no-cache-dir --upgrade pip \
    && /opt/pocket-tts/bin/pip install --no-cache-dir pocket-tts

# Copy binaries, backend, frontend dist, fixtures, and HF Space configs
COPY --from=rust-builder /usr/local/bin/voice-pipes /usr/local/bin/voice-pipes
COPY --from=llama-bin /app/ /opt/llama/
COPY voice-pipes/fixtures /app/fixtures
COPY voice-pipes-admin/backend/app /app/backend/app
COPY --from=frontend-builder /frontend/dist /app/frontend/dist
COPY deploy/huggingface /app/hf

RUN chmod +x /app/hf/entrypoint.sh /app/hf/download-models.sh /app/hf/pocket-tts-start.sh \
    && mkdir -p /app/models /app/data/captures /app/data/audit /app/data/sessions /app/data/state \
    && chown -R user:user /app /opt/llama /opt/pocket-tts

ENV HOME=/home/user \
    PATH="/opt/llama:${PATH}" \
    LD_LIBRARY_PATH="/opt/llama" \
    PYTHONPATH="/app/backend" \
    PYTHONUNBUFFERED="1"

USER user
EXPOSE 7860

HEALTHCHECK --interval=30s --timeout=5s --start-period=180s --retries=3 \
    CMD ["curl", "-sf", "http://127.0.0.1:7860/livez"]

ENTRYPOINT ["/app/hf/entrypoint.sh"]

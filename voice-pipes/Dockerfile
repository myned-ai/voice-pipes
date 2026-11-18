# syntax=docker/dockerfile:1
# (`COPY --parents`, below, needs the 1.x frontend rather than the one built into the engine.)
# Keep in sync with `rust-version` in the workspace Cargo.toml (MSRV) — the
# containerized build is what continuously proves the declared MSRV.
FROM rust:1.91-trixie AS chef

RUN echo 'Acquire::Retries "5";' > /etc/apt/apt.conf.d/80-retries \
    && echo 'Acquire::http::Timeout "60";' >> /etc/apt/apt.conf.d/80-retries \
    && echo 'Acquire::https::Timeout "60";' >> /etc/apt/apt.conf.d/80-retries \
    && (for i in 1 2 3 4 5; do apt-get update && break || sleep 3; done) \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        libasound2-dev \
        ca-certificates \
        cmake \
        clang \
        libclang-dev \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-chef --locked

WORKDIR /app

# Ride out slow crates.io moments instead of failing the whole build: more
# retries, and give a stalled transfer 120s (default 30s) before timing out.
# GGML_NATIVE=OFF ensures llama.cpp / ggml builds portable binaries across all CPUs.
ENV CARGO_NET_RETRY=10 \
    CARGO_HTTP_TIMEOUT=120 \
    GGML_NATIVE=OFF

FROM chef AS planner
# Build context is the repository root (workspace), so the `voice-pipes`
# crate can see its `path = "../voice-pipes-sdk"` dependency.
COPY Cargo.toml Cargo.lock ./
# Every workspace crate, by pattern rather than by name, so a new crate needs
# no edit here: its manifest, sources, build script and protos. `--parents`
# keeps each file's directory, and a pattern a crate does not use (most have
# no build.rs) matches nothing. Only compile inputs are copied, so editing
# docs, configs or tests does not invalidate the build layer.
COPY --parents voice-pipes*/Cargo.toml voice-pipes*/build.rs voice-pipes*/src voice-pipes*/proto plugins/*/*/Cargo.toml plugins/*/*/build.rs plugins/*/*/src plugins/*/*/proto ./
# Compile-time embedded assets outside a crate's src (include_str!): the
# wizard's model registry. Copied file by file so editing runtime configs
# (worker TOMLs) does not invalidate the build layer.
COPY voice-pipes/config/wizard_models.json ./voice-pipes/config/wizard_models.json
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
# The features the published image is built with: the CPU list in
# deploy/docker/docker-compose.yml, so a plain `docker build` produces the
# image that ships. A workspace rule holds the two equal. An explicit empty
# value builds the CLI's default features instead.
ARG FEATURES="realtime,text-grpc,tts-kokoro,tts-pocket,stt-moonshine,tts-openai,tts-elevenlabs,stt-assemblyai,stt-google,tts-gemini,stt-nemotron,stt-nemotron35,turn-smart,tts-neutts,llm-gguf"
ARG CARGO_FLAGS=""

COPY --from=planner /app/recipe.json recipe.json
# Cache mounts survive across builds, so crate downloads and the ort-sys
# prebuilt-onnxruntime download (cdn.pyke.io, ~100MB, prone to stalling when
# builds run in parallel) only ever have to succeed once.
# `sharing=locked`: compose builds several images at once, and two cargo
# runs unpacking the same crate into a shared registry fail with
# "failed to unpack package ... File exists". Locked, the second build
# waits for the first.
# Dependency compilation (`cargo chef cook`) writes into the image layer so CI
# (`type=gha`) caches all third-party crates across releases.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/root/.cache/ort.pyke.io,sharing=locked \
    if [ -n "${FEATURES}" ]; then \
        cargo chef cook --release -p voice-pipes-cli ${CARGO_FLAGS} --features "${FEATURES}" --recipe-path recipe.json; \
    else \
        cargo chef cook --release -p voice-pipes-cli ${CARGO_FLAGS} --recipe-path recipe.json; \
    fi

COPY Cargo.toml Cargo.lock ./
COPY --parents voice-pipes*/Cargo.toml voice-pipes*/build.rs voice-pipes*/src voice-pipes*/proto plugins/*/*/Cargo.toml plugins/*/*/build.rs plugins/*/*/src plugins/*/*/proto ./
COPY voice-pipes/config/wizard_models.json ./voice-pipes/config/wizard_models.json

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/root/.cache/ort.pyke.io,sharing=locked \
    if [ -n "${FEATURES}" ]; then \
        cargo build --release -p voice-pipes-cli ${CARGO_FLAGS} --features "${FEATURES}"; \
    else \
        cargo build --release -p voice-pipes-cli ${CARGO_FLAGS}; \
    fi \
    && cp /app/target/release/voice-pipes /usr/local/bin/voice-pipes

FROM debian:trixie-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libasound2 \
        ca-certificates \
        espeak-ng \
        libgomp1 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /usr/local/bin/voice-pipes /usr/local/bin/voice-pipes

# Probe fixtures, baked in rather than mounted.
#
# `POST /debug/probe/*` runs one component against a known input, and it is
# most wanted on a worker that is already deployed and misbehaving -- which is
# exactly when nobody can add a bind mount and restart. They are ~680 KB of
# 16 kHz mono WAV, so carrying them costs nothing next to the models.
COPY voice-pipes/fixtures /app/fixtures

# Run as an unprivileged user. Bind-mounted data dirs (models,
# captures, audit, sessions, benchmarks) must be writable by this uid; the
# compose file sets `user: "${VP_UID:-10001}:${VP_GID:-10001}"` and the
# launcher exports the invoking user's ids on Linux/macOS, so host-created
# directories keep working without a chown. HOME stays under /app so nothing
# writes to `/` (e.g. huggingface cache / hf_hub fallback).
RUN groupadd --system --gid 10001 vp \
    && useradd --system --uid 10001 --gid vp --home-dir /app --shell /usr/sbin/nologin vp \
    && chown -R vp:vp /app
ENV HOME=/app
USER vp

# Each workspace crate logs under its own target (`voice_pipes` does not cover
# `voice_pipes_stt_google`), so every one that logs is named here -- the binary's
# own `EnvFilter` falls back to `info` when `RUST_LOG` is unset, so a bare `ENV`
# keeps every target loud enough without masking `RUST_LOG` from compose.
ENV RUST_LOG=info,voice_pipes=info,voice_pipes_sdk=info,voice_pipes_net=info,voice_pipes_onnx=info,voice_pipes_bench=info,voice_pipes_gym=info,voice_pipes_cli=info,voice_pipes_filter_deepfilter=info,voice_pipes_llm_gguf=info,voice_pipes_llm_openai_compat=info,voice_pipes_s2s_elevenlabs=info,voice_pipes_s2s_gemini=info,voice_pipes_stt_assemblyai=info,voice_pipes_stt_google=info,voice_pipes_stt_moonshine=info,voice_pipes_stt_nemotron=info,voice_pipes_stt_nemotron35=info,voice_pipes_stt_parakeet=info,voice_pipes_tts_elevenlabs=info,voice_pipes_tts_gemini=info,voice_pipes_tts_kokoro=info,voice_pipes_tts_neutts=info,voice_pipes_tts_openai=info,voice_pipes_tts_piper=info,voice_pipes_tts_pocket=info,voice_pipes_turn_smart=info,voice_pipes_turn_vap=info,voice_pipes_vad_silero=info

EXPOSE 8800 8085 9090

ENTRYPOINT ["voice-pipes"]
CMD ["--help"]

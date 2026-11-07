# syntax=docker/dockerfile:1
# (`COPY --parents`, below, needs the 1.x frontend rather than the one built into the engine.)
# Keep in sync with `rust-version` in the workspace Cargo.toml (MSRV) — the
# containerized build is what continuously proves the declared MSRV.
FROM rust:1.91-trixie AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        libasound2-dev \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Build context is the repository root (workspace), so the `voice-pipes`
# crate can see its `path = "../voice-pipes-sdk"` dependency.
COPY Cargo.toml Cargo.lock ./
# Every workspace crate, by pattern rather than by name, so a new crate needs
# no edit here: its manifest, sources, build script and protos. `--parents`
# keeps each file's directory, and a pattern a crate does not use (most have
# no build.rs) matches nothing. Only compile inputs are copied, so editing
# docs, configs or tests does not invalidate the build layer.
COPY --parents \
    voice-pipes*/Cargo.toml voice-pipes*/build.rs voice-pipes*/src voice-pipes*/proto \
    plugins/*/*/Cargo.toml plugins/*/*/build.rs plugins/*/*/src plugins/*/*/proto \
    ./
# Compile-time embedded assets outside a crate's src (include_str!): the
# wizard's model registry. Copied file by file so editing runtime configs
# (worker TOMLs) does not invalidate the build layer.
COPY voice-pipes/config/wizard_models.json ./voice-pipes/config/wizard_models.json

# Ride out slow crates.io moments instead of failing the whole build: more
# retries, and give a stalled transfer 120s (default 30s) before timing out.
ENV CARGO_NET_RETRY=10 \
    CARGO_HTTP_TIMEOUT=120

# The features the published image is built with: the CPU list in
# deploy/docker/docker-compose.yml, so a plain `docker build` produces the
# image that ships. A workspace rule holds the two equal. An explicit empty
# value builds the CLI's default features instead.
ARG FEATURES="realtime,text-grpc,tts-kokoro,tts-pocket,stt-moonshine,tts-openai,tts-elevenlabs,stt-assemblyai,stt-google,tts-gemini,stt-nemotron,stt-nemotron35,turn-smart"
# Cache mounts survive across builds, so crate downloads and the ort-sys
# prebuilt-onnxruntime download (cdn.pyke.io, ~100MB, prone to stalling when
# builds run in parallel) only ever have to succeed once.
# `sharing=locked`: compose builds several images at once, and two cargo
# runs unpacking the same crate into a shared registry fail with
# "failed to unpack package ... File exists". Locked, the second build
# waits for the first.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/root/.cache/ort.pyke.io,sharing=locked \
    if [ -n "${FEATURES}" ]; then \
        cargo build --release -p voice-pipes-cli --features "${FEATURES}"; \
    else \
        cargo build --release -p voice-pipes-cli; \
    fi

FROM debian:trixie-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libasound2 \
        ca-certificates \
        espeak-ng \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/voice-pipes /usr/local/bin/voice-pipes

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
# tries to write to /root.
RUN groupadd --system --gid 10001 vp \
    && useradd --system --uid 10001 --gid vp --home-dir /app --shell /usr/sbin/nologin vp \
    && chown -R vp:vp /app
ENV HOME=/app
USER vp

# Each workspace crate logs under its own target (`voice_pipes` does not cover
# `voice_pipes_stt_google`), so every one that logs is named here -- the binary's
# own (where a fatal start-up error goes), the pool, the bench extension and
# every plugin. A test holds this list to the workspace.
ENV RUST_LOG=voice_pipes=info,voice_pipes_cli=info,voice_pipes_onnx=info,voice_pipes_bench=info,voice_pipes_gym=info,voice_pipes_stt_parakeet=info,voice_pipes_stt_moonshine=info,voice_pipes_tts_piper=info,voice_pipes_vad_silero=info,voice_pipes_stt_nemotron=info,voice_pipes_stt_nemotron35=info,voice_pipes_stt_google=info,voice_pipes_tts_kokoro=info,voice_pipes_tts_pocket=info,voice_pipes_tts_openai=info,voice_pipes_stt_assemblyai=info,voice_pipes_tts_elevenlabs=info,voice_pipes_tts_gemini=info,voice_pipes_llm_openai_compat=info,voice_pipes_llm_gguf=info,voice_pipes_s2s_openai=info,voice_pipes_s2s_gemini=info,voice_pipes_s2s_elevenlabs=info,voice_pipes_turn_smart=info,voice_pipes_turn_vap=info,ort=warn,h2=warn,hyper=warn,rustls=warn

# Liveness from the binary itself (the image has no HTTP client): a
# worker's transport `/healthz`. The orchestrator, same image on 9090,
# overrides the URL in its compose `healthcheck`. A worker may load models
# for a while before it listens, hence the start period.
HEALTHCHECK --interval=15s --timeout=5s --start-period=120s --retries=3 \
    CMD ["voice-pipes", "healthz"]

ENTRYPOINT ["voice-pipes"]

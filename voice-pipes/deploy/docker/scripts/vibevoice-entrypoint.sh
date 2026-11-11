#!/bin/sh
# VibeVoice-Realtime GPU TTS sidecar entrypoint.
#
# The model weights (models/vibevoice, ~1.1 GB) and reference voice .pt prompts
# (vendor/vibevoice/demo/voices/streaming_model, ~95 MB) are VOLUME-MOUNTED from the repo
# and are empty on a fresh checkout. Download them once into the mounted dirs
# before serving; both persist on the host, so later starts skip straight to the
# server. Downloads need network, so HF offline mode is enabled only for the
# server exec below (not during the download step).
set -e
cd /app

MODEL_DIR=/app/models/vibevoice
VOICES_DIR=/app/vendor/vibevoice/demo/voices/streaming_model

# 1. Model weights — public HF repo, no token needed. config.json is the
#    "already downloaded" marker.
if [ ! -f "$MODEL_DIR/config.json" ]; then
    echo "vibevoice: model weights missing -> downloading into mounted models/ volume (~1.1 GB, first run only)..."
    python scripts/vibevoice/download_model.py
fi

# 2. Reference voice prompts — fetched from GitHub raw (~95 MB).
if [ -z "$(ls -A "$VOICES_DIR"/*.pt 2>/dev/null)" ]; then
    echo "vibevoice: voice prompts missing -> downloading (first run only)..."
    python scripts/vibevoice/download_voices.py
fi

# 3. Serve. HF_HUB_OFFLINE avoids a network handshake on every model load now
#    that the weights are local. server.py auto-detects cuda (the container has a
#    reserved nvidia device) and binds 0.0.0.0:8091.
export HF_HUB_OFFLINE=1
exec python scripts/vibevoice/server.py --model-path models/vibevoice --port 8091

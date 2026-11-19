#!/usr/bin/env bash
set -euo pipefail

# Start Kyutai Pocket-TTS CPU HTTP server on loopback :8002 and fire a warm-up
# synthesis so the first real user turn avoids the ~1.4s cold-start TTFB.

POCKET_ARGS=(serve --host 127.0.0.1 --port 8002)
if [ -n "${POCKET_TTS_CONFIG:-}" ]; then
    POCKET_ARGS+=(--config "${POCKET_TTS_CONFIG}")
fi

/opt/pocket-tts/bin/pocket-tts "${POCKET_ARGS[@]}" &
server_pid=$!

(
python3 - <<'PY'
import time, urllib.parse, urllib.request

body = urllib.parse.urlencode({"text": "warming up the voice pipeline"}).encode()
for _ in range(120):
    try:
        req = urllib.request.Request("http://127.0.0.1:8002/tts", data=body)
        urllib.request.urlopen(req, timeout=30).read()
        print("[pocket-tts] warm-up synthesis complete", flush=True)
        break
    except Exception:
        time.sleep(2)
else:
    print("[pocket-tts] warm-up skipped (server not ready in time)", flush=True)
PY
) || echo "[pocket-tts] warm-up failed (non-fatal)"

wait "${server_pid}"

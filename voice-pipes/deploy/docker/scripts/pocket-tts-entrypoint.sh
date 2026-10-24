#!/bin/sh
# Pocket-TTS sidecar entrypoint with a startup warm-up.
#
# pocket-tts is CPU-only, and the *first* synthesis loads the model into
# the serve process's memory — costing ~1.4s time-to-first-byte on a cold
# start (observed in session 98b0c5f5: ttfb 1394ms cold vs a healthy warm
# rtf of 0.49). Firing one throwaway synthesis at startup means the first
# *real* user turn doesn't pay that cold cost.
#
# The warm-up is best-effort: a failure (e.g. server slow to come up, or
# python missing) must never take down the TTS server itself.

uvx pocket-tts serve --host 0.0.0.0 --port 8000 &
server_pid=$!

# Block until the server answers one synthesis request, then return.
# Retries for ~2 minutes to cover the first-run package + model download.
( python3 - <<'PY'
import time, urllib.request, urllib.parse

body = urllib.parse.urlencode({"text": "warming up the voice pipeline"}).encode()
for _ in range(60):
    try:
        req = urllib.request.Request("http://127.0.0.1:8000/tts", data=body)
        urllib.request.urlopen(req, timeout=30).read()
        print("pocket-tts: warm-up synthesis complete", flush=True)
        break
    except Exception:
        time.sleep(2)
else:
    print("pocket-tts: warm-up skipped (server not ready in time)", flush=True)
PY
) || echo "pocket-tts: warm-up step failed (non-fatal)"

# Hand the foreground back to the server so restart/stop semantics are
# unchanged from the original `uvx pocket-tts serve` command.
wait "$server_pid"

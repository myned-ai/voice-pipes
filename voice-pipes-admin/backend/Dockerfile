# The Python CI tests on and the lock is resolved for (app/core/runtime.py).
FROM python:3.12-slim
WORKDIR /app
COPY requirements.txt .
# The lock pins every package with its hashes; installing anything else fails.
RUN pip install --no-cache-dir --require-hashes -r requirements.txt
COPY app ./app
# Unprivileged runtime user. The backend only reads the bind-mounted
# captures directory; compose sets `user:` so the uid matches the host on
# Linux (see voice-pipes/Dockerfile).
#
# Its own state (the issues feed and the analytics rollup cache, both
# SQLite) goes under /app/state, which compose mounts as a volume. The uid
# the container runs as is not known here, so the directory is
# world-writable with the sticky bit, like /tmp, and the backend makes
# uid-<uid> inside it for itself (app/core/state.py). A fresh volume copies
# this mode.
RUN groupadd --system --gid 10001 vp \
    && useradd --system --uid 10001 --gid vp --home-dir /app --shell /usr/sbin/nologin vp \
    && chown -R vp:vp /app \
    && mkdir -p /app/state \
    && chmod 1777 /app/state
ENV HOME=/app \
    ADMIN_STATE_ROOT=/app/state
USER vp
EXPOSE 8000
# Liveness from the image's own Python (no curl here); /livez needs no token.
HEALTHCHECK --interval=15s --timeout=5s --start-period=20s --retries=3 \
    CMD ["python", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8000/livez', timeout=3)"]
CMD ["uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"]

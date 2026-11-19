# Deployment Plan: Voice Pipes CPU Space under `myned-ai`

This guide walks through creating and deploying the **CPU-only, multi-tenant Voice Pipes pipeline + Admin UI** to Hugging Face Spaces under the **Myned AI (`myned-ai`)** organization account.

---

## 1. Architecture Summary

| Component | Implementation | Hardware |
| :--- | :--- | :--- |
| **Entrypoint** | Nginx (unprivileged, port `7860`) serving React Admin UI + proxying `/api/*` (REST, SSE, WS) | CPU |
| **Admin BFF** | FastAPI (`ADMIN_MULTI_TENANT=true`) with Hugging Face OAuth (`hf_oauth: true`) + signed `HttpOnly; SameSite=None; Secure` tenant cookie | CPU |
| **Orchestrator & Worker** | `voice-pipes` Rust binary (`Silero VAD` → `Moonshine Base STT` → `Pocket-TTS HTTP`) | CPU (`VP_ORT_EP=cpu`) |
| **Local TTS** | Kyutai `pocket-tts` HTTP server on `127.0.0.1:8002` (with startup warm-up, voice `alba`) | CPU |
| **Local LLM** | `llama-server` serving `Qwen2.5-1.5B-Instruct-Q4_K_M.gguf` (Apache 2.0, ungated, `--parallel 4 --cont-batching`) | CPU |
| **Per-User Isolation** | Authoritative cookie-bound `tenant_id` (`hf-<username>`). Sessions, audio, analytics, pipelines, issues, and clear actions are strictly scoped per user. | Built-in |
| **Auto Cleanup** | Background TTL janitor sweeps captures older than **30 min** (`ADMIN_CAPTURE_TTL_MINUTES=30`) + hard **500 MB** cap (`ADMIN_CAPTURES_MAX_MB=500`) + bounded 5–10 MB rotating logs in `supervisord`. | Built-in |

---

## 2. Step-by-Step Plan to Create & Deploy under `myned-ai`

### Step 1: Create the Space on Hugging Face
1. Log in to Hugging Face with an account that has **Write / Admin** access to the **`myned-ai`** organization.
2. Go to **https://huggingface.co/new-space** and configure:
   - **Owner**: `myned-ai`
   - **Space name**: e.g. `voice-pipes-demo` (URL will be `https://huggingface.co/spaces/myned-ai/voice-pipes-demo`)
   - **License**: `apache-2.0`
   - **Select the Space SDK**: **Docker** → **Blank**
   - **Space hardware**:
     - Recommended: **CPU Upgrade — 8 vCPU · 32 GB RAM** (~$0.05/hr, comfortably handles 4–8 parallel users smoothly)
     - Heavy demo load: **CPU Upgrade — 16 vCPU · 64 GB RAM** (~$0.10/hr)
     - *(Note: Free 2 vCPU tier will work for 1 user at a time, but 8 vCPU is strongly recommended for snappy STT + Qwen 2.5 1.5B)*
   - **Visibility**: Public (anyone can sign in with HF and get their own isolated workspace) or Private (Myned AI org members only).
3. Click **Create Space**.

### Step 2: Configure Space Settings (Optional Storage & Variables)
In the Space's **Settings** tab:
1. **Sleep time**: Set **Sleep time** (e.g. `15 minutes` or `1 hour`) so you only pay for compute when someone is actively using it.
2. **Persistent Storage (Optional, ~$5/mo for 20 GB)**:
   - Not required — `entrypoint.sh` automatically detects `/data` if attached and caches the ~1.4 GB of models in `/data/models` so Space restarts skip re-downloading models (~15s faster cold boot).
   - Without persistent storage, models download from Hugging Face Hub on cold start in ~20–30 seconds.
3. **Space Variables** (optional overrides — all have safe defaults):
   - `ADMIN_CAPTURE_TTL_MINUTES=30` — delete saved session audio & logs after 30 minutes
   - `ADMIN_CAPTURES_MAX_MB=500` — hard disk cap for all captures combined
   - `ADMIN_ALLOW_GUEST=false` — set to `true` if you also want a "Continue as Isolated Guest" button alongside "Sign in with Hugging Face"
   - `LLAMA_MODEL_FILE` / `LLAMA_MODEL_URL` — swap the CPU LLM (defaults to `Qwen2.5-1.5B-Instruct-Q4_K_M.gguf`)
   - `POCKET_TTS_CONFIG` — optional Pocket-TTS config URI (e.g. `hf://myned-ai/pocket-tts-greek/greek.yaml` to run Myned AI's custom language models)
4. **Space Secrets** (optional):
   - `ADMIN_SESSION_SECRET` — set a fixed 64-hex secret if you want user login cookies to survive Space restarts (otherwise generated automatically at boot).
   - `HF_TOKEN` — only needed if you swap `LLAMA_MODEL_URL` to a gated model like Gemma 3 1B. (Not needed for the default Apache-2.0 Qwen 2.5 1.5B!)

### Step 3: Prepare & Push the Space Repository
Hugging Face Spaces expects `README.md` (with YAML frontmatter) and `Dockerfile` at the **root** of the Space git repo.

Option A — **Direct push from a staging clone (recommended for clean Space history)**:
```bash
# 1. Clone the empty HF Space repo
git clone https://huggingface.co/spaces/myned-ai/voice-pipes-demo /tmp/hf-space

# 2. Export the current branch of voice-pipes into the Space repo
git archive feat/hf-spaces-cpu-multitenant | tar -x -C /tmp/hf-space

# 3. Place the HF Space README.md and Dockerfile at the Space repo root
cp /tmp/hf-space/deploy/huggingface/SPACE_README.md /tmp/hf-space/README.md
cp /tmp/hf-space/deploy/huggingface/Dockerfile /tmp/hf-space/Dockerfile

# 4. Commit and push to Hugging Face (use an HF write token or SSH key)
cd /tmp/hf-space
git add -A
git commit -m "Deploy Voice Pipes CPU multi-tenant Space"
git push origin main
```

Option B — **Automated GitHub Actions Sync** (future CI):
Add a GitHub Actions workflow triggered on push to `main` that copies `deploy/huggingface/SPACE_README.md` → `README.md` and `deploy/huggingface/Dockerfile` → `Dockerfile` and force-pushes to `https://myned-ai:$HF_TOKEN@huggingface.co/spaces/myned-ai/voice-pipes-demo`.

### Step 4: Verify the Deployment
Once Hugging Face finishes building the Docker image (first build ~8–12 min due to Rust release compile; cached builds are much faster):
1. Open **Logs** in the HF Space UI and confirm:
   - `[models] all CPU pipeline models ready`
   - `admin BFF running in MULTI-TENANT mode (auth_mode=hf_oauth, ...)`
   - `capture janitor started (ttl_minutes=30.0, max_mb=500.0, ...)`
   - `nginx` listening on `7860`
2. Open the Space URL (`https://huggingface.co/spaces/myned-ai/voice-pipes-demo` or direct `https://myned-ai-voice-pipes-demo.hf.space`).
3. Click **Sign in with Hugging Face**:
   - Confirm you land in the Admin UI.
   - Go to **Playground**, select `worker-hf-cpu`, and have a short voice conversation.
   - Go to **Sessions** and **Dashboard** — verify your session, audio playback, and latency metrics appear.
4. **Isolation Check**:
   - Open an Incognito window, sign in with a **second Hugging Face account** (or guest session if enabled).
   - Confirm **Sessions** and **Dashboard** are completely empty — User B cannot see User A's sessions, pipelines, or metrics.
5. **Cleanup Check**:
   - Wait >30 minutes (or temporarily set `ADMIN_CAPTURE_TTL_MINUTES=2` to test) and confirm in Space logs: `capture janitor swept ttl_deleted=1 ...` and the old session's WAV/JSONL artifacts are removed.

---

## 3. Local Docker Test Before Pushing to HF

You can test the exact Hugging Face container locally before pushing:
```bash
# Build from the repository root
docker build -f deploy/huggingface/Dockerfile -t voice-pipes-hf-cpu .

# Run locally on port 7860 (guest mode enabled since HF OAuth env vars are absent)
docker run --rm -p 7860:7860 \
  -e ADMIN_CAPTURE_TTL_MINUTES=10 \
  voice-pipes-hf-cpu
```
Then open **http://localhost:7860** in two separate browser profiles (normal + incognito) to verify isolated sessions, voice playground, and Admin UI metrics!

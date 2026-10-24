# Demo data — seeding capture sessions

The admin dashboard's **Dashboard**, **Session History**, and analytics read from
capture session folders on disk (the `CAPTURES_DIR`, default
`../voice-pipes/captures`). With an empty captures dir the dashboard shows zeros,
so use the seeder below to populate realistic demo sessions for a demo, a
screenshot, or local development.

## Seed

From `voice-pipes-admin/backend/`:

```bash
python scripts/seed_demo_captures.py            # 24 sessions (default)
python scripts/seed_demo_captures.py 50         # any count
python scripts/seed_demo_captures.py 50 /path/to/captures   # custom dir
```

It writes session folders (`metadata.json` + `NNN_turn_resolved.json` +
`events.jsonl`) into the captures dir. **It only adds files — it never deletes.**
Clear and re-seed freely; it is all synthetic demo data.

To reset, delete the captures dir contents and re-seed:

```bash
rm -rf ../voice-pipes/captures/*        # bash
python scripts/seed_demo_captures.py 50
```

## What the data looks like

- Timestamps spread across the **last ~5 days** so the default 7-day window and
  the time-series charts are populated.
- 3 pipelines (`support-en`, `sales-en`, `voicemail`), varied STT/TTS/LLM
  providers (deepgram/parakeet/assemblyai, cartesia/elevenlabs/piper,
  openai/local), 1–4 turns per session.
- A fat latency tail (occasional slow turns) so **percentiles / SLO breaches**
  are visible, plus per-turn token usage and **per-provider cost** (spend chart),
  client TTFA, occasional **audio underruns** and **barge-in** events, and ~12%
  of sessions recording an error (+ ~5% with no turns) so **failed sessions** and
  the **error rate** are non-zero.
- Per-session **business outcomes**: an `outcome` (contained / escalated / abandoned)
  and a `csat` rating (1–5, ~70% of sessions) in `metadata.json`, so the Dashboard's
  **Business · Outcomes** cards (containment rate, escalation rate, avg CSAT) are populated.
- Deterministic: it uses a fixed random seed, so repeated runs at the same count
  produce the same distribution.

## View it

Backend (terminal 1) — points the BFF at the demo captures:

```bash
cd voice-pipes-admin/backend
CAPTURES_DIR="$(pwd)/../../voice-pipes/captures" \
  python -m uvicorn app.main:app --port 8000
# add DEBUG_TIMING=1 and keep the default/info log level to watch the
# per-scan timing + rollup hit/miss line.
```

Frontend (terminal 2):

```bash
cd voice-pipes-admin/frontend
npm run dev          # http://localhost:5173 (API base auto-points at :8000)
```

Or the whole stack via Docker (mounts the captures + frontend automatically):

```bash
cd voice-pipes-admin
docker compose up --build      # frontend http://localhost:3000
```

## Notes

- **Two data sources on the Dashboard.** The KPI cards, audio-quality cards,
  charts, failed-sessions and anomaly/issues feed come from these **captures**
  (file-based analytics — what the seeder fills). The top **"Pipelines &
  Component Health"** panel and **Live Graph** come from the live **orchestrator**
  (`:9090`) and are unaffected by the seeder; they degrade gracefully when the
  orchestrator is down.
- First analytics load parses the seeded sessions once and writes
  `backend/tmp/rollups.db`; reloads (and restarts) serve from that rollup.
  `tmp/*.db` is gitignored.

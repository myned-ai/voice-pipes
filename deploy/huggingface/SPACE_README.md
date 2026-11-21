---
title: Voice Pipes — CPU Voice Pipeline & Admin Console
emoji: 🎙️
colorFrom: indigo
colorTo: purple
sdk: docker
app_port: 7860
license: apache-2.0
hf_oauth: true
hf_oauth_expiration_minutes: 480
pinned: false
short_description: Real-time CPU voice pipeline with per-user isolated Admin UI
---

# Voice Pipes (CPU-Only Multi-Tenant Space)

Interactive low-latency voice assistant and observability console powered by **Voice Pipes** (by Myned AI).

### Pipeline Architecture (100% Local CPU)
- **VAD**: Silero VAD (ONNX CPU)
- **STT**: Moonshine Base (ONNX CPU)
- **LLM**: Qwen 2.5 1.5B Instruct Q4_K_M (`llama-server` CPU, continuous batching)
- **TTS**: Kyutai Pocket-TTS (`pocket-http`, CPU, voice `alba`)

### Privacy, Admission Queue & User Isolation
- Sign in with your **Hugging Face** account (`hf_oauth: true`).
- Your sessions, latency metrics, pipelines, transcripts, and audio recordings are strictly isolated to your own tenant partition (`captures/hf-<username>/`). Other users cannot see your sessions or metrics.
- **Fair-Share Waiting Room & Concurrency Guard**: To keep CPU inference responsive for everyone, up to **3 concurrent live voice sessions** (`1` per user, 5-minute session cap) run simultaneously; additional visitors enter a live FIFO waiting queue with real-time position and ETA updates.
- Saved recordings, capture artifacts, and logs are automatically deleted after **30 minutes** (or when the 500 MB cap is reached) to keep Space storage clean.

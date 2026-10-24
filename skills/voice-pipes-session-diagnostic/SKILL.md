---
name: voice-pipes-session-diagnostic
description: Analyze and diagnose Voice Pipes voice AI pipeline sessions, interpret diagnostic reports, trace latency bottlenecks across STT, LLM, TTS, S2S, and VAD, and recommend performance optimizations.
---

# Voice Pipes Session Diagnostic Skill

Use this skill whenever a user:
1. Pastes a Voice Pipes session diagnostic report (e.g. starting with `# Voice pipeline session diagnostic`, `Verdict: DEGRADED`, or containing JSON telemetry).
2. Asks why a session was graded as `degraded`, `fair`, or `broken`.
3. Asks which component (STT, LLM, TTS, VAD/Turn Detection, or Network) is causing latency bottlenecks.
4. Asks for advice on configuring or tuning Voice Pipes pipeline SLAs, model parameters, or silence timeouts.

---

## 1. Core Physical Principles & Seam Invariants

When analyzing Voice Pipes session telemetry, strictly enforce these physical invariants to avoid false attribution:

### 1.1 The Critical Path (Turn-Taking Latency)
Turn latency is measured from the moment the user stops talking to the moment the first sample of synthesized agent audio reaches the caller:
$$\text{Caller Wait (p90)} = \text{endpointing\_ms} + \text{llm\_wait\_ms} + \text{tts\_internal\_ttfb\_ms} + \text{output\_wait\_ms}$$

### 1.2 Common Traps & Diagnostic Rules
- **TTS TTFB Contains LLM TTFT**:
  `tts.ttfb_ms` is commit-anchored (measured from turn commit $\to$ first audio chunk emitted to client). It already incorporates the full `llm.ttft_ms`!
  To isolate pure TTS latency, use `tts.internal_ttfb_ms` or compute `ttfb_ms - ttft_ms`. **Never blame TTS for high latency when the delay occurred in the LLM.**
- **Batch STT `processing_ms` Spans Utterance Duration**:
  Batch STT engines (e.g. Parakeet TDT in batch mode) measure `processing_ms` from utterance audio start to final transcript. This overlaps user speech; a long user utterance inflates this number without stalling the user. Judge STT turn-taking latency strictly on `endpointing_ms`.
- **Streaming STT Processing**:
  For streaming STT (`stt.mode == "streaming"`), `processing_ms` is `null` by design because chunks are transcribed incrementally.
- **Realtime Pacer & Output RTF**:
  `emit_rtf` is streaming-phase wall time $\div$ synthesized audio duration.
  - If `emit_rtf > 1.0`, the pipeline is starving the client audio buffer (causes dropouts/clicks).
  - If `emit_rtf \le 1.0`, audio flows smoothly. A very fast TTS model (e.g. `rtf = 0.08`) will still report `emit_rtf \approx 0.9` because the engine's realtime pacer intentionally holds chunks to match natural speech playback speed.
- **Echo / Self-Barge Detection**:
  If `interrupted = true`, check `bi.played_ms` and `bi.trigger`.
  If `played_ms < 200` ms and `trigger == "server_vad"`, it is acoustic feedback (the agent's speaker leaked into the microphone), NOT a real human barge-in.
- **Cold-Start Turns**:
  Turn 1 almost always pays threadpool setup, ONNX session initialization, or KV cache allocation overhead. Note it as cold-start, but evaluate steady-state health on subsequent turns.
- **Custom Grading Overrides**:
  Check `report.grading.custom` or the `verdict.custom_grading` block. When an operator configures custom thresholds in `[grading.targets]` (e.g. relaxed budgets for local CPU models), evaluate against the configured targets rather than general cloud standards.

---

## 2. Key Metrics Cheat Sheet

| Metric | Component | Meaning | Target (Fast / Cloud) |
|---|---|---|---|
| `endpointing_ms` | VAD / STT | User silence $\to$ turn commit | $\le 500$ ms |
| `processing_ms` | STT | Inference time (batch mode spans utterance) | $\le 800$ ms |
| `ttft_ms` | LLM | Prompt submission $\to$ first generated token | $\le 300$ ms (local GPU) / $\le 800$ ms (cloud) |
| `tokens_per_sec` | LLM | Generation throughput | $\ge 30$ tok/s |
| `ttfb_ms` | TTS | Commit $\to$ first audio chunk (contains TTFT!) | $\le 400$ ms |
| `tts_internal_ttfb_ms` | TTS | Net TTS time: first text fragment $\to$ first audio chunk | $\le 200$ ms |
| `rtf` | TTS | Synthesis compute time $\div$ audio duration ($< 1.0$ is faster than RT) | $\le 0.60$ |
| `emit_rtf` | Output | Audio send wall time $\div$ audio duration ($> 1.0$ is starvation) | $\le 0.90$ |
| `totalUnderruns` | Client | Buffer starvation count on the client | `0` |

---

## 3. Analysis Workflow

When given a report:
1. **Confirm or Refine the Verdict**:
   Check if the precomputed verdict (HEALTHY, FAIR, DEGRADED, BROKEN) accurately reflects the critical path. If a candidate anomaly is merely off the critical path (e.g. batch STT processing overlapping speech), explain why it should be downgraded to advisory/diagnostic.
2. **Break Down the Critical Path Waterfall**:
   Calculate the latency contribution of each stage for the p90 turn:
   - Endpointing silence: $X$ ms
   - LLM first token: $Y$ ms
   - TTS synthesis: $Z$ ms
3. **Audit Anomalies**:
   For each item in `anomalies[]`:
   - State: `[CONFIRMED]`, `[DISMISSED]`, or `[ADVISORY]`.
   - Provide a concise 1-sentence technical justification.
4. **Actionable Recommendations**:
   Provide 2–3 targeted tuning steps:
   - Config tweaks (`silence_timeout_ms`, buffer sizes, pacer).
   - Execution provider / hardware fixes (ORT CUDA, thread counts).
   - Pipeline model trade-offs (e.g. lighter quantization, streaming STT).

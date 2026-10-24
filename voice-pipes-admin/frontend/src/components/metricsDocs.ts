// Metric legend entries shared by the Playground legend popup and the
// SessionDetail telemetry sidebar (rendered by `MetricsGuide`). Kept in a
// non-component module so `MetricsGuide.tsx` only exports components
// (react-refresh rule).

export interface MetricDoc {
  label: string;
  /** Tailwind text colour for the label; "" = theme default. */
  color: string;
  /** Tailwind background for the legend dot. */
  dot: string;
  title: string;
  desc: string;
}

export const METRIC_DOCS: MetricDoc[] = [
  {
    label: "STT",
    color: "text-sky-400",
    dot: "bg-sky-500",
    title: "Speech-to-Text latency",
    desc: "Time from your audio being flushed (end-of-speech detected by VAD) to receiving the final transcript from the STT engine. For hosted providers like AssemblyAI this includes a network round-trip; for local Whisper/Parakeet it is pure inference time. STT completes at t=0 (utterance commit) — it runs before all other metrics.",
  },
  {
    label: "Endpointing",
    color: "text-sky-400",
    dot: "bg-sky-500",
    title: "End-of-utterance (EOU) silence detection",
    desc: "The trailing silence the VAD waits through before declaring you've finished speaking. Measured from the last frame of detected speech to the moment your audio is flushed to STT. This is pure dead time on the wire — nothing is being transcribed or generated yet — but the user perceives it as part of response latency. Typical production values are 300–700 ms; lower means snappier turn-taking but more accidental cut-offs (false endpoints) when you pause mid-sentence. On the timeline it is shown as the hatched portion of the STT bar labelled 'Silence (EOU)'.",
  },
  {
    label: "TTFT",
    color: "text-amber-400",
    dot: "bg-amber-500",
    title: "Time to First Token (LLM)",
    desc: "Time from utterance commit (t=0) to the very first text token produced by the LLM. Captures prompt-encoding and KV-cache warm-up time. TTFT is always smaller than TTFB because text must arrive before TTS can synthesise any audio.",
  },
  {
    label: "LLM",
    color: "text-amber-400",
    dot: "bg-amber-500",
    title: "LLM generation span",
    desc: "Wall-clock duration of the full streaming LLM generation — from the first token to the last. The pipeline feeds text fragments to TTS as they arrive, so LLM and TTS overlap. A longer response means LLM span > TTFB.",
  },
  {
    label: "TTS",
    color: "text-violet-400",
    dot: "bg-violet-500",
    title: "TTS time-to-first-audio",
    desc: "Time from the first LLM text fragment arriving at the TTS engine to the first audio sample produced. This is why TTFB > TTFT: the pipeline is sequential — LLM first token → TTS startup → first audio. For Piper this is typically 50–200 ms depending on text chunk size.",
  },
  {
    label: "TTFB",
    color: "text-emerald-400",
    dot: "bg-emerald-500",
    title: "Time to First Byte — audio (server)",
    desc: "Time from the end-of-speech anchor to the first audio byte sent to your browser (never the first transcript — see First text). Cascade: anchor = utterance commit (t=0), TTFB ≈ TTFT + TTS. Speech-to-speech: the anchor is whatever the provider or the worker could stamp — `ttfb_anchor_source` says which (provider speech-stopped, vp VAD, client VAD, or the provider's late input-transcript proxy, which understates the wait by 0.45–1.0 s; `turn_start` means no anchor at all and the number is not a latency).",
  },
  {
    label: "First text",
    color: "text-emerald-400",
    dot: "bg-emerald-500",
    title: "Time to first transcript (S2S, diagnostic)",
    desc: "Speech-to-speech only: anchor → the first output-transcript delta from the provider (`first_text_ms`). Tells you when the model *started*; the audio can follow 0.3–1.2 s later because providers such as Gemini buffer their opening audio. Not on the critical path — TTFB is what the user waits for.",
  },
  {
    label: "Endpointing (S2S)",
    color: "text-rose-400",
    dot: "bg-rose-500",
    title: "Provider endpointing lag (speech-to-speech)",
    desc: "How long the speech-to-speech provider's VAD / transcription took to notice you stopped talking: its end-of-speech anchor minus the worker's own end-of-speech stamp (`provider_endpointing_ms`). This is the S2S counterpart of the cascade Endpointing and sits on the critical path — the user waits through it before TTFB even starts. Budget 500 ms; Gemini's input-transcript proxy typically lands 450–1000 ms late.",
  },
  {
    label: "Anchor",
    color: "",
    dot: "bg-slate-400",
    title: "End-of-speech anchor source",
    desc: "Where the turn's t=0 comes from (`ttfb_anchor_source` on the wire, `e2e_anchor_source` in Session History): `client vad` (the browser's own endpointer), `vp vad` (the worker's uplink observer), `provider` (e.g. OpenAI speech_stopped), `transcript proxy` (Gemini's last input-transcript delta — late, shown amber), `text` (typed turn) or `unanchored` (measured from turn start — TTFB ≈ 0 means nothing). Report precedence for E2E: client › vp › provider › proxy.",
  },
  {
    label: "Total",
    color: "",
    dot: "bg-slate-400",
    title: "Wall-clock total (server)",
    desc: "Time from utterance commit to the very last audio byte for this turn — the full pipeline duration. Greater than TTFB whenever the LLM generates more than the first chunk: the server keeps streaming audio until TTS finishes synthesising all LLM output. Example: TTFB 1383 ms, Total 1647 ms → 264 ms of continued streaming after first audio.",
  },
  {
    label: "E2E",
    color: "text-sky-400",
    dot: "bg-sky-400",
    title: "Client end-to-end latency",
    desc: "Measured entirely by your browser: time from the client VAD's end of speech (the same instant the `timing_report speech_end` is sent — the hangover is 300 ms after your last loud frame) to the first audio chunk arriving back. Includes endpointing + STT + LLM TTFT + TTS TTFB + two network legs. This is the closest number to what you actually perceive as 'response delay'. E2E − RTT ≈ pure server processing time. When no VAD stamp exists for a turn (client VAD disabled, or speech ended while the agent was still talking) the pill falls back to the server's `listening` transition — i.e. mic open — and says so in its tooltip; that fallback overstates the wait.",
  },
  {
    label: "TTFA",
    color: "text-sky-400",
    dot: "bg-sky-400",
    title: "Client time-to-first-audio (as heard)",
    desc: "Measured entirely in your browser: client VAD end of speech → the first sample leaving the audio output. Equals E2E plus the client jitter buffer (Buffer), anchored like E2E. Compare with `client_ttfa_ms` in Session History, which is the same quantity computed server-side from the client's timing reports.",
  },
  {
    label: "Buffer",
    color: "",
    dot: "bg-slate-400",
    title: "Jitter buffer + scheduling (client)",
    desc: "How long the first audio of a turn sat in the client before playing: prebuffer hold + AudioContext scheduling (+ output latency where the browser exposes it — Chrome/Firefox; Safari excludes the 10–40 ms device latency). Steady delivery ⇒ ≈ Settings → Prebuffer; bursty delivery ⇒ the safety timeout (2× prebuffer, min 250 ms) shows up here.",
  },
  {
    label: "Delivery",
    color: "",
    dot: "bg-slate-400",
    title: "Server → ear (clock-offset corrected)",
    desc: "Server pacer → ear for the first chunk, using the NTP-lite offset from Ping/Pong. Roughly one network leg + Buffer. Absent until a Pong has been received or when the worker does not stamp `server_timestamp_ms`.",
  },
  {
    label: "RTT",
    color: "",
    dot: "bg-slate-400",
    title: "Network round-trip time",
    desc: "Measured via WebSocket Ping/Pong independently of any turn. Subtract from E2E to estimate how much time was spent on the server vs on the wire. Only shown once a Pong has been received.",
  },
  {
    label: "Emit RTF",
    color: "text-violet-400",
    dot: "bg-violet-500",
    title: "Output realtime factor (server, streaming phase)",
    desc: "Wall time from the first audio chunk the server handed to the transport to the last, divided by the audio duration. Excludes TTFB — it only measures the streaming phase. > 1.0 means the server emitted audio slower than it plays: the client hears it fine until the prebuffer drains, then playback breaks up. Target ≤ 0.9. Compare with the `source_rtf` column in the per-component table (what the provider/TTS delivered on its own): the difference is the post-processor chain.",
  },
  {
    label: "Chain",
    color: "text-violet-400",
    dot: "bg-violet-500",
    title: "Post-processor chain overhead",
    desc: "Wall time (ms) the inline audio post-processors added between the server receiving audio from the provider/TTS and forwarding it to you: their summed call time, capped at `emit_wall_ms − source_wall_ms`. 0 when the turn ran no processor. Budget ≈ 10% of the turn's audio duration. A large value with a normal `source_rtf` means a processor — not the model — is the bottleneck; the per-component table lists each `audio_post:<name>` with its p50 latency and `budget_ratio`.",
  },
  {
    label: "Pacer",
    color: "text-violet-400",
    dot: "bg-violet-500",
    title: "Realtime pacer wait",
    desc: "The rest of `emit_wall_ms − source_wall_ms` after the post-processors' share: mostly the realtime pacer holding audio back to playback speed. A provider that delivers faster than realtime (e.g. `source_rtf` 0.23) is emitted at about 1.0, so a large pacer wait with Emit RTF near 1.0 is by design, not a slow component.",
  },
  {
    label: "Arrival RTF",
    color: "text-sky-400",
    dot: "bg-sky-400",
    title: "Client arrival realtime factor",
    desc: "Measured entirely in your browser: wall time from the first to the last audio frame received, divided by the audio duration received. The client-side counterpart of Emit RTF — it adds the network. > 1.0 with underruns = starvation (the server/network delivered slower than realtime); ≤ 1.0 with underruns = jitter (bursty delivery vs. a small prebuffer). Known only once the turn ends, so it is also reported for interrupted turns.",
  },
];

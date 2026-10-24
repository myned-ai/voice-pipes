// Presentation of the end-of-speech anchor provenance: the wire carries
// `ttfb_anchor_source`
// on the realtime component and Session History carries `e2e_anchor_source`
// on the utterance summary. Both use the same vocabulary; the label map is
// shared by the Playground metrics bar and the Session History turn header.

/** Tone of the anchor badge: `proxy` = late, known-optimistic anchor;
 *  `none` = no anchor at all (the number is not a latency). */
export type AnchorTone = "ok" | "proxy" | "none";

export interface AnchorSourceInfo {
  /** Short badge text, e.g. "transcript proxy". */
  label: string;
  tone: AnchorTone;
  /** One-line explanation for tooltips. */
  detail: string;
}

const KNOWN: Record<string, AnchorSourceInfo> = {
  client_speech_end: {
    label: "client vad",
    tone: "ok",
    detail: "the client's own VAD stamped the end of speech (timing_report speech_end)",
  },
  vp_vad: {
    label: "vp vad",
    tone: "ok",
    detail: "the worker's uplink end-of-speech observer stamped the end of speech",
  },
  provider_speech_stopped: {
    label: "provider",
    tone: "ok",
    detail: "the provider signalled end of speech (e.g. OpenAI input_audio_buffer.speech_stopped)",
  },
  input_transcript_proxy: {
    label: "transcript proxy",
    tone: "proxy",
    detail: "anchored on the provider's last input-transcript delta — lands 0.45–1.0 s after the user actually stopped, so the real wait is longer than shown",
  },
  text_submit: {
    label: "text",
    tone: "ok",
    detail: "the turn was typed — anchored on the text submit",
  },
  turn_start: {
    label: "unanchored",
    tone: "none",
    detail: "no end-of-speech anchor at all — measured from turn start; TTFB ≈ 0 here is not a latency",
  },
};

/** Describe an anchor source string; unknown values fall back to the raw
 *  key (underscores → spaces) so a new backend vocabulary still renders. */
export function describeAnchorSource(source: string): AnchorSourceInfo {
  return KNOWN[source] ?? { label: source.replace(/_/g, " "), tone: "ok", detail: `anchor source: ${source}` };
}

/** Client-side E2E anchor: which stamp `clientE2eMs` was measured from. */
export type ClientE2eSource = "client_vad" | "listening";

export function describeClientE2eSource(source: ClientE2eSource | undefined): string {
  switch (source) {
    case "client_vad":
      return "client VAD end of speech";
    case "listening":
      return "the server's `listening` transition (mic-open fallback — no client VAD stamp for this turn; overstates the wait)";
    default:
      return "no client anchor (agent-initiated turn)";
  }
}

/**
 * Analysis for the microphone check, kept free of Web Audio and React so the
 * judgements can be tested against recorded numbers rather than a live device.
 *
 * The check exists because of a class of fault that is invisible from inside a
 * voice pipeline: the session reports itself as listening, the level meter
 * reads zero, every log line is clean, and the recogniser returns punctuation
 * for a spoken sentence. Three unrelated causes produce that same picture --
 * a capture graph that never ran, a live track delivering digital silence, and
 * browser processing gating speech to near-nothing -- and telling them apart
 * from transcripts alone took days.
 */

/** One analysed block of captured audio. */
export interface Block {
  /** Milliseconds since capture started. */
  t: number;
  /** RMS of the block, 0..1. */
  rms: number;
  /** Largest absolute sample in the block, 0..1. */
  peak: number;
}

export interface Summary {
  blocks: number;
  /** Largest absolute sample seen across every block. */
  peak: number;
  /** Largest block RMS seen. */
  maxRms: number;
  /** Wall time from the first block to the last, in ms. */
  spanMs: number;
  /** Total time spent above the speech floor, in ms. */
  activeMs: number;
  /** Longest single unbroken run above the floor, in ms. */
  longestRunMs: number;
  /** Fraction of the elapsed span that was active, 0..1. */
  activeRatio: number;
}

export type VerdictKind =
  | "waiting"
  | "no-frames"
  | "silent-track"
  | "very-quiet"
  | "clipping"
  | "ok";

export interface Verdict {
  kind: VerdictKind;
  /** `good` reads as pass, `critical` as fail, `warning` as works-but-suspect. */
  status: "good" | "warning" | "critical" | "neutral";
  title: string;
  detail: string;
}

/** How long to let the graph start before judging silence. Covers worklet
 *  module loading and an autoplay-policy resume. */
export const SETTLE_MS = 1500;

/** Block RMS above which audio counts as speech rather than room tone. Set
 *  below a quiet talker (~0.02) and above the noise floor of a muted-but-live
 *  track (~0.0002). */
export const SPEECH_FLOOR_RMS = 0.01;

/** Peak at or above which samples are at the rail. */
const CLIP_PEAK = 0.99;

/** Below this peak there is something, but too little for a recogniser. */
const TOO_QUIET_PEAK = 0.02;

export function summarise(blocks: Block[]): Summary {
  if (blocks.length === 0) {
    return {
      blocks: 0, peak: 0, maxRms: 0, spanMs: 0,
      activeMs: 0, longestRunMs: 0, activeRatio: 0,
    };
  }
  let peak = 0;
  let maxRms = 0;
  let activeMs = 0;
  let longestRunMs = 0;
  let runMs = 0;

  for (let i = 0; i < blocks.length; i++) {
    const b = blocks[i];
    if (b.peak > peak) peak = b.peak;
    if (b.rms > maxRms) maxRms = b.rms;
    // A block covers the time until the next one; the last block is charged
    // the same duration as the one before it rather than zero.
    const next = blocks[i + 1];
    const dt = next ? next.t - b.t : (i > 0 ? b.t - blocks[i - 1].t : 0);
    if (b.rms >= SPEECH_FLOOR_RMS) {
      activeMs += dt;
      runMs += dt;
      if (runMs > longestRunMs) longestRunMs = runMs;
    } else {
      runMs = 0;
    }
  }

  const spanMs = blocks[blocks.length - 1].t - blocks[0].t;
  return {
    blocks: blocks.length,
    peak,
    maxRms,
    spanMs,
    activeMs,
    longestRunMs,
    activeRatio: spanMs > 0 ? activeMs / spanMs : 0,
  };
}

/**
 * Judge what the device is doing, given everything seen so far.
 *
 * `elapsedMs` is wall time since capture started, which is what separates
 * "nothing yet" from "nothing at all": before the graph has had time to start,
 * an empty summary means only that it is early.
 */
export function classify(s: Summary, elapsedMs: number): Verdict {
  if (elapsedMs < SETTLE_MS && s.peak === 0) {
    return {
      kind: "waiting", status: "neutral",
      title: "Starting",
      detail: "Waiting for the first audio from the device.",
    };
  }
  if (s.blocks === 0) {
    return {
      kind: "no-frames", status: "critical",
      title: "No audio blocks at all",
      detail:
        "The capture graph never ran, so nothing was delivered to measure. " +
        "This is a browser or permission fault, not a quiet room.",
    };
  }
  if (s.peak === 0) {
    return {
      kind: "silent-track", status: "critical",
      title: "Track is live but every sample is zero",
      detail:
        `${s.blocks} blocks arrived on schedule and all of them are digital ` +
        "silence. The microphone is usually held by another tab or " +
        "application, or muted in the operating system.",
    };
  }
  if (s.peak < TOO_QUIET_PEAK) {
    return {
      kind: "very-quiet", status: "critical",
      title: "Signal is far too quiet to transcribe",
      detail:
        `Loudest sample is ${s.peak.toFixed(4)} of full scale. Raise the input ` +
        "level in the operating system's sound settings, or move closer to the mic.",
    };
  }
  if (s.peak >= CLIP_PEAK) {
    return {
      kind: "clipping", status: "warning",
      title: "Working, but hitting the rail",
      detail:
        "Samples reach full scale, so the loudest parts are being flattened. " +
        "Lower the input level, or turn off automatic gain control below.",
    };
  }
  return {
    kind: "ok", status: "good",
    title: "Microphone is working",
    detail:
      `Peak ${s.peak.toFixed(3)}, ${(s.activeMs / 1000).toFixed(1)} s of speech ` +
      `across ${s.blocks} blocks. The device and browser are not the problem.`,
  };
}

// ── Comparing two takes ─────────────────────────────────────

export interface Take {
  label: string;
  /** Whether the browser's own processing was enabled for this take. */
  processing: boolean;
  summary: Summary;
}

export interface Comparison {
  status: "good" | "warning" | "critical" | "neutral";
  title: string;
  detail: string;
  /** Speech duration with processing on, as a fraction of it off. */
  keptRatio: number | null;
}

/** Below this fraction of speech surviving, processing is removing words
 *  rather than cleaning them up. */
const GATED_KEPT_RATIO = 0.7;

/** Gain ratio above which processing is clearly amplifying, not just
 *  levelling -- the signature that accompanies aggressive gating. */
const BOOSTED_RMS_RATIO = 1.8;

/**
 * Compare the same phrase spoken twice, once with the browser's processing on
 * and once with it off.
 *
 * This is the part a pipeline cannot do for itself. Echo cancellation, noise
 * suppression and automatic gain run before any application code sees the
 * audio, so their damage is indistinguishable from a person who simply said
 * less. Saying the same thing twice is what makes it measurable: if far less
 * speech survives with processing on, the processing removed it.
 */
export function compare(withProcessing: Take, withoutProcessing: Take): Comparison {
  const on = withProcessing.summary;
  const off = withoutProcessing.summary;

  if (on.blocks === 0 || off.blocks === 0) {
    return {
      status: "neutral", keptRatio: null,
      title: "Need both takes",
      detail: "Record the same phrase with processing on and with it off.",
    };
  }
  if (off.activeMs < 200) {
    return {
      status: "neutral", keptRatio: null,
      title: "Not enough speech to compare",
      detail:
        "The take with processing off holds almost no speech, so there is " +
        "nothing to measure the other against. Say a full sentence in each take.",
    };
  }

  const keptRatio = on.activeMs / off.activeMs;
  const gainRatio = off.maxRms > 0 ? on.maxRms / off.maxRms : 1;

  if (keptRatio < GATED_KEPT_RATIO) {
    const boosted = gainRatio >= BOOSTED_RMS_RATIO;
    return {
      status: "critical", keptRatio,
      title: "Browser processing is removing your speech",
      detail:
        `Only ${(keptRatio * 100).toFixed(0)}% of the speech survives with ` +
        `processing on (${(on.activeMs / 1000).toFixed(2)} s against ` +
        `${(off.activeMs / 1000).toFixed(2)} s)` +
        (boosted
          ? `, and what is left is ${gainRatio.toFixed(1)}× louder — automatic gain ` +
            "is amplifying one burst while noise suppression gates the rest. "
          : ". ") +
        "A recogniser receives a fragment instead of a sentence. Turn the " +
        "browser's processing off for this pipeline.",
    };
  }

  return {
    status: "good", keptRatio,
    title: "Browser processing is not eating your speech",
    detail:
      `${(keptRatio * 100).toFixed(0)}% of the speech survives with processing ` +
      "on, so it is safe to leave enabled. If transcripts are still wrong, the " +
      "cause is downstream of capture.",
  };
}

// Tunables and fixed values of the session core.

import type { QualityTier } from "../../transport/types";

/** Sanity bound for client playback timings: anything outside [0, 60 s] is a
 *  stale clock offset or a broken clock, not a measurement. */
export const PLAYBACK_METRIC_MAX_MS = 60_000;

// SDK configuration constants avoiding hardcoded values
export const DEFAULT_MIC_SAMPLE_RATE = 48000;
export const DEFAULT_PLAYBACK_SAMPLE_RATE = 48000;
export const DEFAULT_PING_INTERVAL_MS = 5000;

/** How often to send `net_report` (S3). Frequent enough to react during a long
 *  turn — which per-turn `playback_quality` cannot — and cheap enough to
 *  ignore: roughly 150 bytes per report, ~0.6 kbps. */
export const NET_REPORT_INTERVAL_MS = 2000;

/** Jitter-buffer depth per quality tier, ms.
 *
 *  A degraded link wants a *bigger* cushion, not a smaller one: the symptom
 *  is late, uneven arrival, and buffer is what absorbs it.
 *
 *  These are a deliberate trade against time-to-first-audio, the metric the
 *  rest of this SDK exists to minimise — so the ladder is conservative and
 *  overridable rather than baked in. Raises apply from the *next* utterance
 *  (see `setPrebufferMs`), so the benefit lands on the turn after the one that
 *  went badly; that is intentional, since re-holding audio already flowing
 *  would insert the very gap the buffer prevents. */
export const DEFAULT_PREBUFFER_BY_TIER: Record<QualityTier, number> = {
  // `full` matches DEFAULT_PREBUFFER_MS: the tier ladder must never *reduce*
  // the cushion below what a healthy link already needs. It was 200 here and
  // that value demonstrably underran on a bursting S2S provider (session
  // 5fbf767c) — see the note on DEFAULT_PREBUFFER_MS.
  full: 400,
  reduced: 600,
  minimal: 800,
  critical: 1000,
};
export const INITIAL_SEQUENCE_NUMBER = 0;
export const ERROR_TRANSPORT_CODE = 500;

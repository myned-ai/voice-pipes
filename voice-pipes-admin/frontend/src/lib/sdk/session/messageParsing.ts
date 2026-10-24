// Pure readers for the realtime fields of a `PipelineMessage` text delta.

import type { PipelineMessage } from "../../transport/types";

/** Audio offset (ms into the utterance) a realtime text delta is anchored
 *  at: `payload.audio_ms` when present, else a `timestamp_ms` small enough
 *  to be an offset rather than a wall-clock epoch; null when neither. */
export function realtimeDeltaAudioMs(pm: PipelineMessage): number | null {
  const payload = (pm.payload ?? null) as { audio_ms?: number } | null;
  return typeof payload?.audio_ms === "number"
    ? payload.audio_ms
    : (typeof pm.timestamp_ms === "number" && pm.timestamp_ms < 10000000000)
      ? pm.timestamp_ms
      : null;
}

/** Utterance id of a realtime text delta, from `turn_id` (`rt-<n>`) or
 *  else `message_id` (`rt-asst-<n>-…`); -1 when neither carries one. */
export function realtimeDeltaUtteranceId(pm: PipelineMessage): number {
  let uttId = -1;
  if (pm.turn_id?.startsWith("rt-")) {
    const n = parseInt(pm.turn_id.slice(3), 10);
    if (Number.isFinite(n)) uttId = n;
  }
  if (uttId < 0 && pm.message_id?.startsWith("rt-asst-")) {
    const rest = pm.message_id.slice("rt-asst-".length);
    const n = parseInt(rest.split("-")[0], 10);
    if (Number.isFinite(n)) uttId = n;
  }
  return uttId;
}

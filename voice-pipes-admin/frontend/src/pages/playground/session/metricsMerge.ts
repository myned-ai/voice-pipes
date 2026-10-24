// Pure merge of a late `playbackMetrics` event into the turn-metrics list.
// Kept out of the hook so the upsert is unit-testable without React.

import type { ClientPlaybackMetrics } from "../../../lib/sdk/VoicePipesSessionCore";
import type { TurnMetricsEntry } from "./types";

/** Copy the client playback timings onto the entry for `m.utteranceId`.
 *  Returns the SAME array when no entry matches (the `turn_metrics` for that
 *  utterance has not arrived yet — the core merges the timings into its
 *  envelope instead). A later call for the same utterance overwrites. */
export function mergePlaybackMetrics(entries: TurnMetricsEntry[], m: ClientPlaybackMetrics): TurnMetricsEntry[] {
  const i = entries.findIndex((e) => e.snapshot.utterance_id === m.utteranceId);
  if (i < 0) return entries;
  const next = entries.slice();
  next[i] = {
    ...next[i],
    clientReceiveToPlayMs: m.clientReceiveToPlayMs,
    clientTtfaMs: m.clientTtfaMs,
    clientPlaybackLatencyMs: m.clientPlaybackLatencyMs,
    clientArrivalRtf: m.clientArrivalRtf,
    // The anchor the E2E/TTFA were measured from. Keep the envelope's value
    // when the playback event carries none (agent-initiated turn, or a late
    // event after the core already cleared its per-turn stamp).
    clientE2eSource: m.clientE2eSource ?? next[i].clientE2eSource,
  };
  return next;
}

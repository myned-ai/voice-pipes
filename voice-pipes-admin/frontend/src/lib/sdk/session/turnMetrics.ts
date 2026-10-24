// Pure assembly of a `turn_metrics` frame into the envelope listeners
// receive, and the one-line summary written to the event log.

import type { TurnMetricsSnapshot } from "../../transport/types";
import type { ClientE2eSource } from "../../metrics/anchorSource";
import {
  audioPostComponents,
  chainOverheadMs,
  pacerWaitMs,
  componentShortName,
  emitRtf,
  extraNumber,
  firstTextMs,
  llmGenerationMs,
  providerEndpointingMs,
  sourceRtf,
  sttLatencyMs,
  ttfbAnchorSource,
  ttftMs,
  ttfbMs,
  ttsTtfbMs,
} from "../../metrics/lookup";
import type { ClientPlaybackMetrics, TurnMetricsEnvelope } from "./types";

/** Client-side measurements merged into a turn's metrics. `pm` is the
 *  utterance's playback timings, or null when its first chunk has not been
 *  scheduled yet. */
export interface ClientTurnTimings {
  clientE2eMs: number | undefined;
  clientE2eSource: ClientE2eSource | undefined;
  pm: ClientPlaybackMetrics | null;
}

export function describeTurnMetrics(snap: TurnMetricsSnapshot, client: ClientTurnTimings): string {
  const { clientE2eMs, clientE2eSource, pm } = client;
  // Output throughput: the streaming
  // phase's realtime factors plus one `post[<name>]` cell per inline
  // post-processor — the numbers that make "slower than realtime" visible
  // in a log that is otherwise first-byte oriented.
  const post = audioPostComponents(snap)
    .map((c) => ` post[${componentShortName(c)}]=p50 ${extraNumber(c, "latency_p50_ms") ?? "-"}ms ratio ${extraNumber(c, "budget_ratio") ?? "-"}`)
    .join("");
  return `metrics: total=${snap.total_ms}ms stt=${sttLatencyMs(snap) ?? "-"}ms ttft=${ttftMs(snap) ?? "-"}ms llm=${llmGenerationMs(snap) ?? "-"}ms tts=${ttsTtfbMs(snap) ?? "-"}ms ttfb=${ttfbMs(snap) ?? "-"}ms${ttfbAnchorSource(snap) ? ` (anchor ${ttfbAnchorSource(snap)})` : ""} first_text=${firstTextMs(snap) ?? "-"}ms endpointing=${providerEndpointingMs(snap) ?? "-"}ms e2e=${clientE2eMs ?? "-"}ms${clientE2eMs !== undefined && clientE2eSource ? ` (anchor ${clientE2eSource})` : ""} ttfa=${pm?.clientTtfaMs ?? "-"}ms buffer=${pm?.clientReceiveToPlayMs ?? "-"}ms delivery=${pm?.clientPlaybackLatencyMs ?? "-"}ms emit_rtf=${emitRtf(snap) ?? "-"} source_rtf=${sourceRtf(snap) ?? "-"} chain=${chainOverheadMs(snap) ?? "-"}ms pacer=${pacerWaitMs(snap) ?? "-"}ms${post}${snap.interrupted ? " (interrupted)" : ""}`;
}

export function buildTurnMetricsEnvelope(
  snap: TurnMetricsSnapshot,
  rttMs: number | null,
  client: ClientTurnTimings,
): TurnMetricsEnvelope {
  const { clientE2eMs, clientE2eSource, pm } = client;
  return {
    snapshot: snap,
    rttMs: rttMs ?? undefined,
    clientE2eMs,
    ...(clientE2eMs !== undefined && clientE2eSource ? { clientE2eSource } : {}),
    ...(pm
      ? {
          clientReceiveToPlayMs: pm.clientReceiveToPlayMs,
          clientTtfaMs: pm.clientTtfaMs,
          clientPlaybackLatencyMs: pm.clientPlaybackLatencyMs,
          clientArrivalRtf: pm.clientArrivalRtf,
        }
      : {}),
  };
}

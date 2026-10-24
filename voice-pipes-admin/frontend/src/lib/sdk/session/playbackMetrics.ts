// Pure computations behind the client playback telemetry: first-chunk
// timings, the per-turn arrival realtime factor, and the periodic
// `net_report` deltas. The session core owns the state and the clocks and
// passes their readings in.

import type { PlaybackQuality } from "../../transport/types";
import type { ClientE2eSource } from "../../metrics/anchorSource";
import type { ClientPlaybackMetrics } from "./types";
import { PLAYBACK_METRIC_MAX_MS } from "./constants";

/** Time from now until the first sample of `chunk` is audible, ms:
 *  `chunk.startTime − ctx.currentTime` is the remaining hold before the
 *  sample plays; `ctx.outputLatency` (Chrome/Firefox only — Safari reports
 *  nothing, so the number then excludes the 10–40 ms output-device latency)
 *  is added on top. */
export function untilAudibleMs(chunkStartTime: number, ctx: AudioContext): number {
  const outputLatency = (ctx as AudioContext & { outputLatency?: number }).outputLatency ?? 0;
  return (chunkStartTime - ctx.currentTime + outputLatency) * 1000;
}

/** Server-clock readings for the delivery metric; absent when the chunk
 *  carries no server timestamp or no Pong sample exists yet. */
export interface DeliveryClock {
  nowWallMs: number;
  clockOffsetMs: number;
  serverTimestampMs: number;
}

/** The client playback timings of an utterance's first scheduled chunk. */
export function firstChunkPlaybackMetrics(args: {
  utteranceId: number;
  untilAudibleMs: number;
  nowPerfMs: number;
  arrivalPerfMs: number;
  clientE2eMs: number | undefined;
  clientE2eSource: ClientE2eSource | undefined;
  delivery: DeliveryClock | null;
}): ClientPlaybackMetrics {
  const clamp = (v: number): number | undefined =>
    Number.isFinite(v) && v >= 0 && v <= PLAYBACK_METRIC_MAX_MS ? Math.round(v) : undefined;

  const clientReceiveToPlayMs = clamp(args.nowPerfMs + args.untilAudibleMs - args.arrivalPerfMs);
  const clientTtfaMs =
    args.clientE2eMs !== undefined && clientReceiveToPlayMs !== undefined
      ? args.clientE2eMs + clientReceiveToPlayMs
      : undefined;
  const clientE2eSource = clientTtfaMs !== undefined ? args.clientE2eSource : undefined;
  const d = args.delivery;
  // server ≈ client + offset; a negative result means the offset is stale,
  // not that audio played before it was sent — the clamp drops it.
  const clientPlaybackLatencyMs =
    d !== null
      ? clamp(d.nowWallMs + args.untilAudibleMs + d.clockOffsetMs - d.serverTimestampMs)
      : undefined;

  return {
    utteranceId: args.utteranceId,
    clientReceiveToPlayMs,
    clientTtfaMs,
    clientPlaybackLatencyMs,
    ...(clientE2eSource !== undefined ? { clientE2eSource } : {}),
  };
}

export function describePlaybackTimings(m: ClientPlaybackMetrics): string {
  const clientE2eSource = m.clientE2eSource;
  return `playback timings: receive→play=${m.clientReceiveToPlayMs ?? "-"}ms ttfa=${m.clientTtfaMs ?? "-"}ms delivery=${m.clientPlaybackLatencyMs ?? "-"}ms${clientE2eSource ? ` (anchor: ${clientE2eSource})` : ""}`;
}

export function describePlaybackQuality(utteranceId: number, q: PlaybackQuality, interrupted: boolean): string {
  return (
    `playback quality utt=${utteranceId}: audio=${q.audioMs}ms arrival=${q.arrivalWallMs}ms underruns=${q.underrunCount} silence=${q.totalSilenceMs}ms` +
    `${q.annotationLateCount > 0 ? ` late_annotations=${q.annotationLateCount} (max ${q.annotationLateMsMax}ms)` : ""}${interrupted ? " (interrupted)" : ""}`
  );
}

/** `prev` (when it belongs to this utterance) with `clientArrivalRtf` set
 *  from the turn's `playback_quality` counters, 2 decimals. */
export function withArrivalRtf(
  prev: ClientPlaybackMetrics | null,
  utteranceId: number,
  q: PlaybackQuality,
): ClientPlaybackMetrics {
  const clientArrivalRtf = Math.round((q.arrivalWallMs / q.audioMs) * 100) / 100;
  const own = prev?.utteranceId === utteranceId ? prev : null;
  return { ...(own ?? { utteranceId }), clientArrivalRtf };
}

/** Cumulative playback counters at a `net_report`, so the next report can
 *  carry a delta rather than a running total. */
export interface NetSnapshot {
  audioMs: number;
  underruns: number;
  atMs: number;
}

/** The playback half of a `net_report`, as deltas since `prev`, plus the
 *  snapshot to keep for the next report (`null` = keep the previous one). */
export function netReportDeltas(
  q: PlaybackQuality | null,
  prev: NetSnapshot | null,
  now: number,
): {
  report: { playback_buffer_ms: number; underruns: number; audio_received_ms: number; wall_ms: number };
  snapshot: NetSnapshot | null;
} {
  let audioReceivedMs = 0;
  let underruns = 0;
  let wallMs = 0;
  let snapshot: NetSnapshot | null = null;
  if (q) {
    snapshot = { audioMs: q.audioMs, underruns: q.underrunCount, atMs: now };
    if (prev && q.audioMs >= prev.audioMs && q.underrunCount >= prev.underruns) {
      audioReceivedMs = q.audioMs - prev.audioMs;
      underruns = q.underrunCount - prev.underruns;
      wallMs = Math.round(now - prev.atMs);
    }
    // A decrease means the counters reset at a turn boundary; re-baseline
    // and report nothing this tick rather than a negative delta.
  }

  return {
    report: {
      // Delivered but not yet heard — what stands between us and a break.
      playback_buffer_ms: q ? Math.max(0, Math.round(q.audioMs - q.heardMs)) : 0,
      underruns,
      audio_received_ms: audioReceivedMs,
      wall_ms: wallMs,
    },
    snapshot,
  };
}

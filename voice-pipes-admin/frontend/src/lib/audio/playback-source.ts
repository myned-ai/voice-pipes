// AudioPlaybackSource — adapts AudioPlaybackService to the PlaybackSource
// interface consumed by DefaultPlaybackClock.
//
// Keeps the playback service free of clock concerns: the service tracks
// audio-context wall time and buffer scheduling; this adapter projects
// those numbers into the {nowMs, totalMs, utteranceId, isRunning,
// scheduledAheadMs} contract.

import type { AudioPlaybackService } from "./playback";
import type { PlaybackSource } from "./playback-clock";

/** Build a `PlaybackSource` view over an `AudioPlaybackService` ref.
 *
 *  A ref (rather than a direct service instance) is used because the
 *  service is recreated when the negotiated output sample rate changes
 *  mid-session.  The adapter always reads the *current* service so it
 *  survives that swap without needing to be torn down and rebuilt.
 *
 *  Returns `null`-tolerant accessors: when the ref points at `null`
 *  (pre-connect, mid-recreate) every getter returns a safe zero. */
export function createPlaybackSource(
  ref: { current: AudioPlaybackService | null },
): PlaybackSource {
  return {
    getNowMs(): number {
      return ref.current?.getPlayedDurationMs() ?? 0;
    },

    getTotalMs(): number {
      const pb = ref.current;
      if (!pb) return 0;
      const playedMs = pb.getPlayedDurationMs();
      const aheadMs = pb.getBufferStats().scheduledAheadMs;
      const computed = playedMs + aheadMs;
      // Fall back to totalEnqueuedDurationMs while the very first chunk is
      // still being scheduled (playedMs and aheadMs are both 0 then).
      return computed > 0 ? computed : pb.totalEnqueuedDurationMs;
    },

    getUtteranceId(): number | null {
      const id = ref.current?.currentUtteranceId;
      // Server utterance IDs can be 0 (e.g. initial greeting). We must NOT
      // treat 0 as "no active utterance". Treat exactly -1 or undefined as null.
      if (id === undefined || id === -1) return null;
      return id;
    },

    isRunning(): boolean {
      return ref.current?.playing ?? false;
    },

    getScheduledAheadMs(): number {
      return ref.current?.getBufferStats().scheduledAheadMs ?? 0;
    },
  };
}

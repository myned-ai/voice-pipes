import { SILENCE_LENIENCY_THRESHOLD_MS, type WordTimingTable } from "../../audio/word-timing-table";

/** Index of the last word heard after `playedMs` of an utterance, for
 *  trimming the transcript on a flush; null when the table is missing or
 *  empty, or no word was heard yet. */
export function trimWordIndex(table: WordTimingTable | undefined, playedMs: number): number | null {
  if (table && table.wordCount > 0) {
    const lastWord = table.timings[table.wordCount - 1];

    // Calculate threshold to tolerate echo-canceller / VAD roundtrip delays and silent tails
    const isNearEnd = playedMs >= table.totalDurationMs - SILENCE_LENIENCY_THRESHOLD_MS;

    // Detect if the playhead has started playing the last word (with 50ms buffer)
    const hasStartedLastWord = playedMs >= lastWord.start_ms - 50;

    if (isNearEnd || hasStartedLastWord) {
      return table.wordCount - 1;
    }

    const wordsHeard = table.wordsHeardAtMs(playedMs);
    return wordsHeard > 0 ? wordsHeard - 1 : null;
  }
  return null;
}

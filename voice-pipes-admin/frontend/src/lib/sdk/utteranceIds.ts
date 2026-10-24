/**
 * Predicting the id of the spoken turn that is about to start.
 *
 * The client files its own timing reports -- `speech_start`, `speech_end` --
 * and has to name the turn they belong to before the server has announced
 * it. So it predicts: one past the highest spoken id it has seen, or the
 * server's stated base when it has seen none.
 *
 * Both halves of that were once wrong, and both corrupted the health report's
 * response-time figures without any error anywhere:
 *
 *  - The base was assumed rather than read. Cascade sessions counted from 0
 *    while this assumed 1, so the first turn's speech reports were filed
 *    under the second turn, and the second turn's response time was measured
 *    from the first turn's end of speech -- 8.3 s reported for a turn that
 *    took 1.8 s. The server now numbers every session from one base and
 *    states it in `session_ready`; this reads it.
 *
 *  - Typed-text turns are numbered from a range of their own, far above the
 *    spoken ones. Counting them moved the prediction past ten thousand after
 *    a single typed message, mislabelling every spoken turn after it. They
 *    are left out of the count.
 */

/** What servers send, and what is assumed of one too old to say. */
export const DEFAULT_FIRST_UTTERANCE_ID = 1;
export const DEFAULT_TEXT_UTTERANCE_ID_BASE = 10_000;

export class UtteranceIdPredictor {
  private firstId = DEFAULT_FIRST_UTTERANCE_ID;
  private textBase = DEFAULT_TEXT_UTTERANCE_ID_BASE;
  /** Highest *spoken* id seen; null until one has been. */
  private maxSeen: number | null = null;

  /** Adopt the numbering the server stated in `session_ready`. */
  configure(negotiated: {
    first_utterance_id?: number;
    text_utterance_id_base?: number;
  }): void {
    if (Number.isFinite(negotiated.first_utterance_id)) {
      this.firstId = negotiated.first_utterance_id as number;
    }
    if (Number.isFinite(negotiated.text_utterance_id_base)) {
      this.textBase = negotiated.text_utterance_id_base as number;
    }
  }

  /** Record an id the server used. Typed-text ids do not count. */
  note(id: number | null | undefined): void {
    if (id == null || !Number.isFinite(id)) return;
    if (id > this.textBase) return;
    if (this.maxSeen === null || id > this.maxSeen) this.maxSeen = id;
  }

  /** The id the next spoken turn will be given. */
  next(): number {
    return this.maxSeen === null ? this.firstId : this.maxSeen + 1;
  }

  /** Forget everything seen; a new session starts its numbering again. */
  reset(): void {
    this.maxSeen = null;
    this.firstId = DEFAULT_FIRST_UTTERANCE_ID;
    this.textBase = DEFAULT_TEXT_UTTERANCE_ID_BASE;
  }
}

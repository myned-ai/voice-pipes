import { describe, it, expect } from "vitest";
import { UtteranceIdPredictor } from "./utteranceIds";

describe("UtteranceIdPredictor", () => {
  it("predicts the server's stated base for the first turn", () => {
    const p = new UtteranceIdPredictor();
    p.configure({ first_utterance_id: 1, text_utterance_id_base: 10_000 });
    expect(p.next()).toBe(1);
  });

  it("reads the base rather than assuming it", () => {
    // The original fault: the base was assumed. Against a server counting
    // from 0 the first turn was predicted as 1, so its speech reports were
    // filed under the second turn and the second turn's response time was
    // measured from the first turn's end of speech.
    const p = new UtteranceIdPredictor();
    p.configure({ first_utterance_id: 0 });
    expect(p.next()).toBe(0);
    p.note(0);
    expect(p.next()).toBe(1);
  });

  it("gives the first and second turns different ids", () => {
    // Stated directly because it is exactly what went wrong: both turns'
    // speech reports carried id 1.
    const p = new UtteranceIdPredictor();
    p.configure({ first_utterance_id: 1 });
    const first = p.next();
    p.note(first);
    const second = p.next();
    expect(second).not.toBe(first);
    expect([first, second]).toEqual([1, 2]);
  });

  it("assumes the documented base of a server too old to state one", () => {
    const p = new UtteranceIdPredictor();
    p.configure({});
    expect(p.next()).toBe(1);
  });

  it("does not count typed-text turns toward the next spoken id", () => {
    // Typed turns are numbered from 10,001. Counting one moved the prediction
    // past ten thousand and mislabelled every spoken turn after it.
    const p = new UtteranceIdPredictor();
    p.configure({ first_utterance_id: 1, text_utterance_id_base: 10_000 });
    p.note(1);
    p.note(10_001);
    expect(p.next()).toBe(2);
    p.note(2);
    p.note(10_002);
    expect(p.next()).toBe(3);
  });

  it("ignores ids that arrive out of order or are not ids", () => {
    const p = new UtteranceIdPredictor();
    p.note(3);
    p.note(2);
    p.note(null);
    p.note(undefined);
    p.note(Number.NaN);
    expect(p.next()).toBe(4);
  });

  it("starts again on reset", () => {
    const p = new UtteranceIdPredictor();
    p.configure({ first_utterance_id: 1 });
    p.note(7);
    p.reset();
    expect(p.next()).toBe(1);
  });
});

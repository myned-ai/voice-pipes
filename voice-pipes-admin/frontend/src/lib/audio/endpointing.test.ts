import { describe, expect, it } from "vitest";
import { DEFAULT_ENDPOINTING, EnergyEndpointer, type EndpointingEvent } from "./endpointing";

// Tests the client energy endpointer against synthetic frame sequences.
// Frames are 10 ms apart; "loud" = 0.1 RMS, "quiet" = 0.0, "mid" sits
// between the exit and enter thresholds.

const LOUD = 0.1;
const QUIET = 0.0;
const MID = 0.015; // exit 0.012 < mid < enter 0.02
const FRAME_MS = 10;

/** Feed `n` frames of `rms` starting at `t0`, collecting events with their
 *  timestamps. Returns the next frame time. */
function run(
  vad: EnergyEndpointer,
  frames: { rms: number; count: number }[],
  t0 = 0,
): { events: { event: EndpointingEvent; atMs: number }[]; nextMs: number } {
  const events: { event: EndpointingEvent; atMs: number }[] = [];
  let t = t0;
  for (const f of frames) {
    for (let i = 0; i < f.count; i++) {
      const ev = vad.feed(f.rms, t);
      if (ev) events.push({ event: ev, atMs: t });
      t += FRAME_MS;
    }
  }
  return { events, nextMs: t };
}

describe("EnergyEndpointer", () => {
  it("defaults: enter 0.02 / exit 0.012 / hangover 300 / minSpeech 150", () => {
    expect(new EnergyEndpointer().options).toEqual(DEFAULT_ENDPOINTING);
    expect(DEFAULT_ENDPOINTING).toEqual({ enterThreshold: 0.02, exitThreshold: 0.012, hangoverMs: 300, minSpeechMs: 150 });
  });

  it("emits speech_start after minSpeechMs of speech and speech_end after the hangover", () => {
    const vad = new EnergyEndpointer();
    const { events } = run(vad, [
      { rms: LOUD, count: 50 }, // 500 ms of speech, t = 0..490
      { rms: QUIET, count: 40 }, // silence from t = 500
    ]);
    expect(events).toEqual([
      { event: "speech_start", atMs: 150 },
      // last speech frame at 490; hangover 300 → end fires at the first frame ≥ 790
      { event: "speech_end", atMs: 790 },
    ]);
    expect(vad.state).toBe("idle");
    expect(vad.inSpeech).toBe(false);
    expect(vad.lastSpeechAtMs).toBe(490);
  });

  it("speech resuming inside the hangover cancels the trailing state (one utterance, one end)", () => {
    const vad = new EnergyEndpointer();
    const { events } = run(vad, [
      { rms: LOUD, count: 30 }, // 0..290
      { rms: QUIET, count: 20 }, // 300..490 — a 200 ms pause, shorter than the hangover
      { rms: LOUD, count: 30 }, // 500..790
      { rms: QUIET, count: 40 }, // 800..
    ]);
    expect(events.map((e) => e.event)).toEqual(["speech_start", "speech_end"]);
    expect(events[1].atMs).toBe(790 + 300);
  });

  it("a pause longer than the hangover ends the utterance; the next segment starts anew", () => {
    const vad = new EnergyEndpointer();
    const { events } = run(vad, [
      { rms: LOUD, count: 30 },
      { rms: QUIET, count: 40 }, // 400 ms > hangover
      { rms: LOUD, count: 30 },
      { rms: QUIET, count: 40 },
    ]);
    expect(events.map((e) => e.event)).toEqual(["speech_start", "speech_end", "speech_start", "speech_end"]);
  });

  it("rejects bursts shorter than minSpeechMs without any event", () => {
    const vad = new EnergyEndpointer();
    const { events } = run(vad, [
      { rms: LOUD, count: 10 }, // 100 ms click
      { rms: QUIET, count: 60 },
    ]);
    expect(events).toEqual([]);
    expect(vad.state).toBe("idle");
  });

  it("hysteresis: a level between exit and enter keeps speech alive but does not start it", () => {
    const vad = new EnergyEndpointer();
    // Mid-level noise alone never enters speech.
    expect(run(vad, [{ rms: MID, count: 50 }]).events).toEqual([]);
    expect(vad.state).toBe("idle");
    // Once in speech, mid-level frames count as speech (no trailing).
    const { events } = run(vad, [
      { rms: LOUD, count: 20 },
      { rms: MID, count: 30 }, // still speech
      { rms: QUIET, count: 40 },
    ], 1000);
    expect(events).toEqual([
      { event: "speech_start", atMs: 1150 },
      // last speech frame (mid) at 1490 → end at 1790
      { event: "speech_end", atMs: 1790 },
    ]);
  });

  it("a mid-level frame during the hangover does not resume speech", () => {
    const vad = new EnergyEndpointer();
    const { events } = run(vad, [
      { rms: LOUD, count: 30 }, // 0..290
      { rms: QUIET, count: 5 }, // 300..340 — enters trailing
      { rms: MID, count: 40 }, // 350.. — below enter: trailing continues
    ]);
    expect(events).toEqual([
      { event: "speech_start", atMs: 150 },
      { event: "speech_end", atMs: 590 },
    ]);
  });

  it("honours custom thresholds / hangover / minSpeech and clamps exit ≤ enter", () => {
    const vad = new EnergyEndpointer({ enterThreshold: 0.5, exitThreshold: 0.9, hangoverMs: 100, minSpeechMs: 0 });
    expect(vad.options).toEqual({ enterThreshold: 0.5, exitThreshold: 0.5, hangoverMs: 100, minSpeechMs: 0 });
    const { events } = run(vad, [
      { rms: 0.6, count: 5 },
      { rms: 0.0, count: 20 },
    ]);
    // minSpeech 0 → start on the very first frame; hangover 100 → end at 40 + 100.
    expect(events).toEqual([
      { event: "speech_start", atMs: 0 },
      { event: "speech_end", atMs: 140 },
    ]);
  });

  it("reset() drops an in-flight segment so no stale speech_end fires later", () => {
    const vad = new EnergyEndpointer();
    run(vad, [{ rms: LOUD, count: 30 }]);
    expect(vad.inSpeech).toBe(true);
    vad.reset();
    expect(vad.state).toBe("idle");
    expect(vad.inSpeech).toBe(false);
    expect(vad.lastSpeechAtMs).toBe(0);
    const { events } = run(vad, [{ rms: QUIET, count: 100 }], 300);
    expect(events).toEqual([]);
  });
});

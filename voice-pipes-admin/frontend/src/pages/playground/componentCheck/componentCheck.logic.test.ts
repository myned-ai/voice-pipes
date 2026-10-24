import { describe, it, expect } from "vitest";
import {
  assessLlm, assessStt, assessTts, assessVad, formatReport, summariseRun,
  type SttResult, type TtsResult, type VadResult,
} from "./componentCheck.logic";

/** A VAD result with nothing wrong, which individual tests then spoil. */
function vad(over: Partial<VadResult> = {}): VadResult {
  return {
    backend: "silero",
    input_duration_ms: 1880,
    sample_rate: 16000,
    pre_roll_ms: 300,
    silence_timeout_ms: 700,
    total_forwarded_ms: 1700,
    total_discarded_ms: 180,
    utterances: [{
      seq: 1, onset_ms: 480, end_ms: 1700,
      forwarded_speech_chunks: 65, forwarded_non_speech_chunks: 20,
      pre_roll_ms_kept: 300, audio_discarded_before_onset_ms: 180,
      speech_discarded_before_onset_ms: 0,
    }],
    ...over,
  };
}

function stt(over: Partial<SttResult> = {}): SttResult {
  return {
    provider: "parakeet-tdt",
    transcript: "Hello, how are you?",
    input_duration_ms: 1880,
    processing_ms: 106,
    rtfx: 17.8,
    empty: false,
    ...over,
  };
}

describe("assessVad", () => {
  it("passes an utterance whose beginning survived", () => {
    // The measured numbers for the hello-how-are-you fixture.
    expect(assessVad(vad(), true).status).toBe("good");
  });

  it("fails when enough speech was dropped to lose the first word", () => {
    const a = assessVad(
      vad({
        pre_roll_ms: 100,
        utterances: [{
          ...vad().utterances[0],
          audio_discarded_before_onset_ms: 390,
          speech_discarded_before_onset_ms: 300,
        }],
      }),
      true,
    );
    expect(a.status).toBe("critical");
    expect(a.summary).toMatch(/of speech was dropped/);
    expect(a.hint).toMatch(/pre_roll_ms/);
  });

  it("does not fail on lead-in silence, however long", () => {
    // The false alarm this replaced, measured on a real recording: a second
    // of quiet before the speaker began was reported as "900 ms was cut from
    // the front of the utterance -- enough to lose the first word". Dropping
    // silence costs nothing; only dropped speech is a fault.
    const a = assessVad(
      vad({
        utterances: [{
          ...vad().utterances[0],
          audio_discarded_before_onset_ms: 900,
          speech_discarded_before_onset_ms: 0,
        }],
      }),
      true,
    );
    expect(a.status).toBe("good");
    expect(a.summary).toMatch(/900 ms of lead-in silence/);
  });

  it("treats no utterance as correct for audio with no speech, and wrong otherwise", () => {
    const silent = vad({ utterances: [], total_forwarded_ms: 0, total_discarded_ms: 2500 });
    expect(assessVad(silent, false).status).toBe("good");
    expect(assessVad(silent, true).status).toBe("critical");
  });

  it("fails when room tone is segmented into a turn", () => {
    // Segmenting silence into an utterance is what feeds a recogniser the
    // input it hallucinates from.
    expect(assessVad(vad(), false).status).toBe("critical");
  });
});

describe("assessStt", () => {
  it("fails a transcript with no words in it, and owns it when given the whole input", () => {
    const a = assessStt(stt({ transcript: "..", empty: true }), "Hello, how are you?");
    expect(a.status).toBe("critical");
    expect(a.hint).toMatch(/this is the recogniser/i);
  });

  it("passes an empty transcript when nothing was said", () => {
    expect(assessStt(stt({ transcript: "", empty: true }), "").status).toBe("good");
  });

  it("fails words invented from silence", () => {
    const a = assessStt(stt({ transcript: "a little bit of the three years ago" }), "");
    expect(a.status).toBe("critical");
    expect(a.summary).toMatch(/invented/i);
  });

  it("owns a lost first word when it was given the whole input", () => {
    // The component check transcribes the whole recording, not what the VAD
    // forwarded, so nothing upstream removed the word. This is how a decoder
    // fault that dropped words after any stretch of silence was found:
    // "The quick brown fox..." came back "x jumps over the lazy dog" from the
    // complete recording.
    const a = assessStt(
      stt({ transcript: "x jumps over the lazy dog" }),
      "The quick brown fox jumps over the lazy dog.",
    );
    expect(a.status).toBe("critical");
    expect(a.summary).toMatch(/first word/i);
    expect(a.hint).toMatch(/not segmentation/i);
  });

  it("points at the segmenter only when it was fed the segmenter's output", () => {
    const a = assessStt(
      stt({ transcript: "o, how are you?" }),
      "Hello, how are you?",
      { inputWasSegmented: true },
    );
    expect(a.hint).toMatch(/segmenter may have cut it/i);
  });

  it("ignores case and punctuation when comparing", () => {
    expect(assessStt(stt({ transcript: "hello how are you" }), "Hello, how are you?").status)
      .toBe("good");
  });

  it("warns when a third of the expected words are missing from the middle", () => {
    // First word intact, so this is not the lost-onset failure; words are
    // simply dropped. Kept distinct because the two have different causes and
    // send you to different components.
    const a = assessStt(
      stt({ transcript: "I was try the other one" }),
      "I was thinking that we could maybe try the other one",
    );
    expect(a.status).toBe("warning");
    expect(a.summary).toMatch(/missing/);
  });

  it("notes when recognition is slower than real time", () => {
    const a = assessStt(stt({ rtfx: 0.4 }), "Hello, how are you?");
    expect(a.hint).toMatch(/slower than real time/i);
  });
});

describe("assessTts", () => {
  const base: TtsResult = {
    provider: "piper", chars: 44, audio_duration_ms: 2554,
    synthesis_ms: 227, rtf: 0.089, sample_rate: 22050, audio_file: "x.wav",
  };
  it("passes comfortable synthesis", () => {
    expect(assessTts(base).status).toBe("good");
  });
  it("fails synthesis slower than real time", () => {
    expect(assessTts({ ...base, rtf: 1.3 }).status).toBe("critical");
  });
  it("warns over the budget but under real time", () => {
    expect(assessTts({ ...base, rtf: 0.75 }).status).toBe("warning");
  });
  it("fails when no audio comes back", () => {
    expect(assessTts({ ...base, audio_duration_ms: 0 }).status).toBe("critical");
  });
});

describe("assessLlm", () => {
  it("passes a prompt reply inside the budget", () => {
    expect(assessLlm({ reply: "I'm well, thanks.", ttft_ms: null, total_ms: 431, chars: 17 }).status)
      .toBe("good");
  });
  it("warns on a reply slow enough to be heard as a pause", () => {
    // The CPU model this replaced took ~16 s for one turn.
    const a = assessLlm({ reply: "ok", ttft_ms: null, total_ms: 15674, chars: 2 });
    expect(a.status).toBe("warning");
    expect(a.hint).toMatch(/gpu/i);
  });
  it("fails an empty reply", () => {
    expect(assessLlm({ reply: "   ", ttft_ms: null, total_ms: 100, chars: 0 }).status)
      .toBe("critical");
  });
});

describe("summariseRun", () => {
  const ok = { status: "good" as const, summary: "fine" };
  const bad = { status: "critical" as const, summary: "broken" };

  it("says so when nothing has run", () => {
    expect(summariseRun([]).status).toBe("neutral");
  });

  it("passes a run where every stage did its job", () => {
    expect(summariseRun([
      { kind: "vad", assessment: ok }, { kind: "stt", assessment: ok },
    ]).status).toBe("good");
  });

  it("names the earliest failing stage in pipeline order, not the worst", () => {
    // Pipeline order is the order a live session meets faults in: speech the
    // VAD drops never reaches the recogniser.
    const s = summariseRun([
      { kind: "stt", assessment: bad },
      { kind: "vad", assessment: { status: "warning", summary: "cut 390 ms" } },
    ]);
    expect(s.headline).toMatch(/^VAD/);
    expect(s.headline).toMatch(/cut 390 ms/);
  });

  it("reports the order stages run in, whatever order results arrive", () => {
    const s = summariseRun([
      { kind: "tts", assessment: bad },
      { kind: "llm", assessment: bad },
    ]);
    expect(s.headline).toMatch(/^LLM/);
  });
});

describe("formatReport", () => {
  const at = new Date("2026-09-21T01:00:00.000Z");
  const base = {
    pipeline: "english-turn-corpus",
    workerId: "worker-english-corpus",
    fixture: "mid-sentence-pause",
    fixtureText: "I was thinking that we could, maybe, try the other one.",
    prompt: "Hello, how are you?",
    generatedAt: at,
  };

  it("carries the input, so the report reproduces without the screen", () => {
    // A verdict with no record of what produced it cannot be acted on by
    // anyone who was not looking at the dialog when it ran.
    const text = formatReport({
      ...base,
      stages: [{
        kind: "vad", label: "VAD",
        assessment: { status: "good", summary: "1 utterance(s)." },
        detail: "silero · pre-roll 300 ms",
      }],
    });
    expect(text).toContain("english-turn-corpus");
    expect(text).toContain("worker-english-corpus");
    expect(text).toContain("mid-sentence-pause");
    expect(text).toContain("I was thinking");
    expect(text).toContain("2026-09-21T01:00:00.000Z");
    expect(text).toContain("silero · pre-roll 300 ms");
  });

  it("marks a fixture that holds no speech, which changes what passing means", () => {
    const text = formatReport({ ...base, fixtureText: "", stages: [] });
    expect(text).toContain("(no speech)");
  });

  it("writes each stage's verdict, reasoning and raw line", () => {
    const text = formatReport({
      ...base,
      stages: [
        {
          kind: "vad", label: "VAD",
          assessment: { status: "good", summary: "onset kept." },
          detail: "silero",
        },
        {
          kind: "stt", label: "STT",
          assessment: {
            status: "critical",
            summary: 'First word is "ing", expected "i".',
            hint: "This one is the recogniser.",
          },
          detail: "parakeet-tdt · 163 ms",
        },
      ],
    });
    expect(text).toMatch(/ok\s+VAD/);
    expect(text).toMatch(/FAIL\s+STT/);
    expect(text).toContain('First word is "ing"');
    expect(text).toContain("This one is the recogniser.");
  });

  it("ends on the same verdict the dialog shows", () => {
    const text = formatReport({
      ...base,
      stages: [
        { kind: "stt", label: "STT", assessment: { status: "critical", summary: "broken" } },
        { kind: "vad", label: "VAD", assessment: { status: "warning", summary: "cut 390 ms" } },
      ],
    });
    // Earliest failing stage, exactly as summariseRun decides it on screen.
    expect(text.trimEnd().endsWith("VAD is the first stage to fail: cut 390 ms")).toBe(true);
  });

  it("reports a stage that errored rather than dropping it", () => {
    const text = formatReport({
      ...base,
      stages: [{ kind: "llm", label: "LLM", error: "worker unreachable: timed out" }],
    });
    expect(text).toContain("FAIL LLM");
    expect(text).toContain("worker unreachable: timed out");
  });

  it("says so when nothing has run", () => {
    expect(formatReport({ ...base, stages: [] })).toContain("No stage has been run yet.");
  });
});

describe("formatReport worker provenance", () => {
  const base = {
    pipeline: "english-turn-corpus",
    workerId: "worker-english-corpus",
    fixture: "hello-how-are-you",
    fixtureText: "Hello, how are you?",
    prompt: "Hello, how are you?",
    generatedAt: new Date("2026-09-21T01:00:00.000Z"),
    stages: [],
  };

  it("says when the worker is the one the session is on", () => {
    expect(formatReport({ ...base, workerSource: "session" }))
      .toContain("the one this Playground dispatched to");
  });

  it("warns when the worker was chosen by dispatch instead", () => {
    // Dispatch returns the LEAST LOADED worker, so a session in trouble makes
    // its own worker the least likely to be chosen. A report that does not
    // say which worker ran can be a clean bill of health for the wrong one.
    const text = formatReport({ ...base, workerSource: "dispatch" });
    expect(text).toContain("least loaded");
    expect(text).toContain("may not be the session's");
  });
});

describe("formatReport for a microphone recording", () => {
  const base = {
    pipeline: "english-turn-corpus",
    workerId: "worker-english-corpus",
    workerSource: "session" as const,
    fixture: "microphone recording, 2.4 s",
    prompt: "Hello, how are you?",
    generatedAt: new Date("2026-09-21T01:00:00.000Z"),
  };

  it("does not call an unstated recording silent", () => {
    // Empty text on a fixture means the fixture holds no speech; on a
    // recording it only means nobody typed what they said. Conflating the two
    // would make every mic report claim the input was silence.
    const text = formatReport({ ...base, fixtureText: "", recorded: true, stages: [] });
    expect(text).toContain("what was said not stated");
    expect(text).not.toContain("(no speech)");
  });

  it("carries each stage's verbatim output beside its verdict", () => {
    const text = formatReport({
      ...base, fixtureText: "Hello, how are you?", recorded: true,
      ttsText: "The quick brown fox.",
      stages: [{
        kind: "stt", label: "STT",
        output: "o, how are you?",
        assessment: { status: "critical", summary: 'First word is "o", expected "hello".' },
      }],
    });
    expect(text).toContain('output: "o, how are you?"');
    expect(text).toContain('tts text:  "The quick brown fox."');
  });
});

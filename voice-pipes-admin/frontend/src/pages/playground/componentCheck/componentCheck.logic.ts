/**
 * Reading probe results, kept free of React and HTTP so the judgements can be
 * tested against recorded responses rather than a live worker.
 *
 * A probe returns numbers; what a person needs is whether the stage did its
 * job. The two are not the same, and the gap is where a day went: a recogniser
 * returning `".."` in 106 ms looks excellent by every latency measure and is a
 * total failure, while a VAD that quietly drops 390 ms off the front of every
 * utterance looks like nothing at all.
 */

export type ProbeKind = "vad" | "stt" | "tts" | "llm";

export type Status = "good" | "warning" | "critical" | "neutral";

export interface Assessment {
  status: Status;
  /** One line, in the terms of the job the stage does. */
  summary: string;
  /** What to do or look at next, when there is something. */
  hint?: string;
}

// ── VAD ─────────────────────────────────────────────────────

export interface VadUtterance {
  seq: number;
  onset_ms: number;
  end_ms: number;
  forwarded_speech_chunks: number;
  forwarded_non_speech_chunks: number;
  pre_roll_ms_kept: number;
  /** All audio dropped ahead of the utterance -- mostly lead-in silence. */
  audio_discarded_before_onset_ms: number;
  /** Of that, how much carried speech. The figure that decides the verdict. */
  speech_discarded_before_onset_ms: number;
}

export interface VadResult {
  backend: string;
  input_duration_ms: number;
  sample_rate: number;
  pre_roll_ms: number;
  silence_timeout_ms: number;
  utterances: VadUtterance[];
  total_forwarded_ms: number;
  total_discarded_ms: number;
}

/**
 * Audio lost before the onset, past which a word is probably gone.
 *
 * An unstressed first syllable runs 150-250 ms, so a quarter second is the
 * point where "hello" starts arriving as "o". Below it the loss is leading
 * silence, which costs nothing.
 */
export const LOST_WORD_MS = 250;

export function assessVad(r: VadResult, expectsSpeech: boolean): Assessment {
  if (r.utterances.length === 0) {
    return expectsSpeech
      ? {
          status: "critical",
          summary: "No utterance found — the recogniser would receive nothing at all.",
          hint:
            "The detector never fired on this audio. Check the input level, then " +
            "the VAD threshold.",
        }
      : {
          status: "good",
          summary: "No utterance, which is correct for audio with no speech in it.",
          hint: "A stage that segments silence into a turn makes the recogniser hallucinate.",
        };
  }
  if (!expectsSpeech) {
    return {
      status: "critical",
      summary: `Found ${r.utterances.length} utterance(s) in audio that holds no speech.`,
      hint: "Room tone reaching the recogniser is what produces invented transcripts.",
    };
  }

  // Judged on speech dropped, never on audio dropped. An earlier version used
  // the latter and reported the second of quiet before someone started
  // talking as "900 ms cut -- enough to lose the first word".
  const lostSpeech = Math.max(...r.utterances.map((u) => u.speech_discarded_before_onset_ms));
  const leadIn = Math.max(...r.utterances.map((u) => u.audio_discarded_before_onset_ms));
  if (lostSpeech >= LOST_WORD_MS) {
    return {
      status: "critical",
      summary:
        `${lostSpeech} ms of speech was dropped before the detector fired — enough ` +
        `to lose the first word.`,
      hint:
        `Pre-roll is ${r.pre_roll_ms} ms; raise [vad] pre_roll_ms, or lower the ` +
        `detection threshold if the speech is quiet.`,
    };
  }
  return {
    status: "good",
    summary:
      `${r.utterances.length} utterance(s), ${r.total_forwarded_ms} ms forwarded; ` +
      `${lostSpeech} ms of speech dropped before the onset` +
      (leadIn > lostSpeech ? ` (plus ${leadIn - lostSpeech} ms of lead-in silence).` : "."),
    hint: "The segmenter would give the recogniser the whole utterance.",
  };
}

// ── STT ─────────────────────────────────────────────────────

export interface SttResult {
  provider: string;
  transcript: string;
  input_duration_ms: number;
  processing_ms: number;
  rtfx: number;
  empty: boolean;
}

/** Normalise for comparison: case, punctuation and spacing are not the test. */
function words(s: string): string[] {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9\s]/g, " ")
    .split(/\s+/)
    .filter(Boolean);
}

/**
 * Where the recogniser's audio came from, which decides what a failure means.
 *
 * The component check feeds the recogniser the *whole* input, not what the
 * segmenter forwarded -- that is what isolates it, and it is how a decoder
 * fault that dropped words after any stretch of silence was found. A word
 * missing from that transcript is therefore the recogniser's own, whatever
 * the VAD row says. Only a recogniser fed the segmenter's output can have
 * had a word removed before it saw it.
 */
export interface SttContext {
  /** True when the audio is what the VAD forwarded; false or absent when it
   *  is the whole input, as in the component check. */
  inputWasSegmented?: boolean;
}

export function assessStt(
  r: SttResult,
  expected?: string,
  context: SttContext = {},
): Assessment {
  const expectsSpeech = expected === undefined || expected.trim() !== "";

  const ownFault = !context.inputWasSegmented;
  if (r.empty) {
    return expectsSpeech
      ? {
          status: "critical",
          summary: "Returned no words.",
          hint: ownFault
            ? "Speech went in and nothing came back, from the whole input rather " +
              "than a fragment of it — this is the recogniser."
            : "Speech went in and punctuation came back; check what the segmenter " +
              "passed on before blaming the recogniser.",
        }
      : {
          status: "good",
          summary: "Returned nothing, which is correct for audio with no speech in it.",
          hint: "A recogniser that invents words from room tone is worse than one that stays quiet.",
        };
  }
  if (!expectsSpeech) {
    return {
      status: "critical",
      summary: `Invented ${JSON.stringify(r.transcript)} from audio with no speech in it.`,
      hint: "This is hallucination, and it reaches the model as if it were said.",
    };
  }

  if (expected) {
    const want = words(expected);
    const got = words(r.transcript);
    if (want.length && got.length) {
      // The first word is checked on its own because losing it is the specific
      // failure this whole surface was built to catch, and a whole-transcript
      // score barely moves when one word of six goes missing.
      if (got[0] !== want[0]) {
        return {
          status: "critical",
          summary: `First word is "${got[0]}", expected "${want[0]}".`,
          hint: ownFault
            ? "The recogniser was given the whole input, so the word was there and it " +
              "dropped it. This is the recogniser, not segmentation."
            : "The beginning did not survive. The segmenter may have cut it; check " +
              "how much speech it dropped before the onset.",
        };
      }
      const missing = want.filter((w) => !got.includes(w)).length;
      if (missing / want.length > 0.34) {
        return {
          status: "warning",
          summary: `${missing} of ${want.length} expected words are missing.`,
          hint: "Compare against the same fixture on another provider before blaming this one.",
        };
      }
    }
  }
  return {
    status: "good",
    summary: `${JSON.stringify(r.transcript)} in ${Math.round(r.processing_ms)} ms.`,
    hint: r.rtfx < 1 ? "Slower than real time — it cannot keep up with a live speaker." : undefined,
  };
}

// ── TTS ─────────────────────────────────────────────────────

export interface TtsResult {
  provider: string;
  chars: number;
  audio_duration_ms: number;
  synthesis_ms: number;
  rtf: number;
  sample_rate: number;
  audio_file: string | null;
}

/** Above this realtime factor, synthesis cannot stay ahead of playback. */
export const RTF_BUDGET = 0.6;

export function assessTts(r: TtsResult): Assessment {
  if (r.audio_duration_ms === 0) {
    return { status: "critical", summary: "Produced no audio." };
  }
  if (r.rtf > 1) {
    return {
      status: "critical",
      summary: `Slower than real time (rtf ${r.rtf.toFixed(2)}) — playback will starve.`,
    };
  }
  if (r.rtf > RTF_BUDGET) {
    return {
      status: "warning",
      summary: `rtf ${r.rtf.toFixed(2)}, above the ${RTF_BUDGET} budget.`,
      hint: "Works, but leaves little headroom when the machine is busy.",
    };
  }
  return {
    status: "good",
    summary:
      `${(r.audio_duration_ms / 1000).toFixed(2)} s of audio in ` +
      `${Math.round(r.synthesis_ms)} ms (rtf ${r.rtf.toFixed(2)}).`,
  };
}

// ── LLM ─────────────────────────────────────────────────────

export interface LlmResult {
  reply: string;
  ttft_ms: number | null;
  total_ms: number;
  chars: number;
}

/** A spoken turn that takes longer than this is felt as a stall. */
export const REPLY_BUDGET_MS = 2000;

export function assessLlm(r: LlmResult): Assessment {
  if (!r.reply.trim()) {
    return { status: "critical", summary: "Returned an empty reply." };
  }
  if (r.total_ms > REPLY_BUDGET_MS) {
    return {
      status: "warning",
      summary: `Replied in ${(r.total_ms / 1000).toFixed(1)} s, which is heard as a pause.`,
      hint: "On CPU this is usually the device, not the model — check it is on the GPU.",
    };
  }
  return {
    status: "good",
    summary: `Replied in ${Math.round(r.total_ms)} ms, ${r.chars} characters.`,
  };
}

// ── Reading a run as a whole ────────────────────────────────

export interface RunSummary {
  status: Status;
  headline: string;
}

const RANK: Record<Status, number> = { good: 0, neutral: 1, warning: 2, critical: 3 };

// ── Writing a run down ──────────────────────────────────────

export interface ReportStage {
  kind: ProbeKind;
  label: string;
  assessment?: Assessment;
  /** The provider/timing line shown under each stage. */
  detail?: string;
  /** What the stage produced, verbatim: the transcript, the reply. Kept apart
   *  from the verdict because a verdict about a transcript is no substitute
   *  for reading it. */
  output?: string;
  error?: string;
}

export interface ReportInput {
  pipeline: string;
  workerId: string | null;
  /** How that worker was chosen. Which worker ran the check is part of the
   *  finding: dispatch returns the least loaded one, which is not necessarily
   *  the one a troubled session is on. */
  workerSource?: "session" | "dispatch" | null;
  fixture: string;
  /** What the fixture's audio actually says; empty means it holds no speech. */
  fixtureText: string;
  /** True when the audio was recorded from the microphone rather than a
   *  fixture. Then an empty `fixtureText` means "not stated", not "silence". */
  recorded?: boolean;
  prompt: string;
  /** Text given to the TTS stage. */
  ttsText?: string;
  stages: ReportStage[];
  generatedAt: Date;
}

const MARK: Record<Status, string> = {
  good: "ok",
  warning: "warn",
  critical: "FAIL",
  neutral: "-",
};

/**
 * Render a run as plain text that can be pasted anywhere.
 *
 * Everything needed to act on it without the screen it came from: which
 * pipeline and worker, which fixture and what that fixture says, and for each
 * stage the verdict, the reasoning and the raw provider line. A report that
 * omits the input is not reproducible, and a screenshot of one cannot be
 * searched, diffed or quoted.
 */
export function formatReport(input: ReportInput): string {
  const ran = input.stages.filter((s) => s.assessment || s.error);
  const lines: string[] = [];

  lines.push("Component check");
  lines.push(`  pipeline:  ${input.pipeline || "(default)"}`);
  lines.push(
    `  worker:    ${input.workerId ?? "(unresolved)"}` +
      (input.workerSource === "session"
        ? " (the one this Playground dispatched to)"
        : input.workerSource === "dispatch"
          ? " (chosen by the orchestrator, least loaded — may not be the session's)"
          : ""),
  );
  lines.push(
    `  ${input.recorded ? "input:  " : "fixture:"}   ${input.fixture}` +
      (input.fixtureText
        ? ` — "${input.fixtureText}"`
        : input.recorded
          ? " — (what was said not stated)"
          : " — (no speech)"),
  );
  lines.push(`  prompt:    ${JSON.stringify(input.prompt)}`);
  if (input.ttsText !== undefined) {
    lines.push(`  tts text:  ${JSON.stringify(input.ttsText)}`);
  }
  lines.push(`  run at:    ${input.generatedAt.toISOString()}`);
  lines.push("");

  if (ran.length === 0) {
    lines.push("  No stage has been run yet.");
    return lines.join("\n");
  }

  for (const s of ran) {
    if (s.error) {
      lines.push(`  ${MARK.critical.padEnd(4)} ${s.label}`);
      lines.push(`       error: ${s.error}`);
      lines.push("");
      continue;
    }
    const a = s.assessment!;
    lines.push(`  ${MARK[a.status].padEnd(4)} ${s.label}`);
    if (s.output !== undefined) lines.push(`       output: ${JSON.stringify(s.output)}`);
    lines.push(`       ${a.summary}`);
    if (a.hint) lines.push(`       ${a.hint}`);
    if (s.detail) lines.push(`       ${s.detail}`);
    lines.push("");
  }

  const summary = summariseRun(
    ran.flatMap((s) => (s.assessment ? [{ kind: s.kind, assessment: s.assessment }] : [])),
  );
  lines.push(`  ${summary.headline}`);
  return lines.join("\n");
}

/**
 * Reduce the stages that ran into one line, naming the earliest broken stage.
 *
 * Earliest in pipeline order, because that is the order a live session meets
 * them in: speech the VAD drops never reaches the recogniser, so a VAD fault
 * is the one a caller hits first. This is an ordering, not a claim that the
 * later stage's fault is caused by the earlier one -- in the component check
 * each stage is probed on its own input, so each verdict is its own.
 */
export function summariseRun(
  stages: { kind: ProbeKind; assessment: Assessment }[],
): RunSummary {
  if (stages.length === 0) {
    return { status: "neutral", headline: "Nothing has been run yet." };
  }
  const order: ProbeKind[] = ["vad", "stt", "llm", "tts"];
  const sorted = [...stages].sort(
    (a, b) => order.indexOf(a.kind) - order.indexOf(b.kind),
  );
  const firstBad = sorted.find((s) => RANK[s.assessment.status] >= RANK.warning);
  if (!firstBad) {
    return {
      status: "good",
      headline: `All ${stages.length} stage(s) did their job.`,
    };
  }
  const label = firstBad.kind.toUpperCase();
  return {
    status: firstBad.assessment.status,
    headline: `${label} is the first stage to fail: ${firstBad.assessment.summary}`,
  };
}

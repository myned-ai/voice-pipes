import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import axios from "axios";
import {
  AlertTriangle, Check, CheckCircle2, Circle, Copy, Mic, Play, Square, X, XCircle,
} from "lucide-react";
import { useTheme } from "../../../contexts/ThemeContext";
import { AudioCaptureService } from "../../../lib/audio/capture";
import {
  assessLlm, assessStt, assessTts, assessVad, summariseRun,
  formatReport,
  type Assessment, type LlmResult, type ProbeKind, type Status,
  type SttResult, type TtsResult, type VadResult,
} from "./componentCheck.logic";
import { concatBlocks, encodeWav } from "./wav";

/**
 * Run one pipeline stage at a time against a known input.
 *
 * The companion to the microphone check: that one asks whether the device is
 * handing over usable audio, this one asks what each stage then does with it.
 * Together they cover the question that took a day to answer by hand -- given
 * a session that transcribes nothing, which stage is at fault.
 *
 * Each stage is measured on its own. In particular the recogniser is given the
 * whole input, not what the VAD forwarded -- that isolation is the point, and
 * it is what exposed a decoder fault that dropped words after any stretch of
 * silence. A transcript failure here is the recogniser's; a VAD failure here
 * is speech a live session's recogniser would never receive.
 *
 * Audio comes from a committed fixture or from the microphone. A fixture is
 * reproducible and removes the device from the question; a recording answers
 * it for the device in front of you. The recording is made through the same
 * capture path a session uses -- deliberately, where the microphone check
 * deliberately avoids it -- so what is probed is what a session would send.
 */

interface Fixture {
  name: string;
  file: string;
  text: string;
  note: string;
  duration_ms: number;
  synthetic: boolean;
}

export interface ComponentCheckDialogProps {
  apiBase: string;
  /** Pipeline whose worker should be probed, as chosen in the Playground. */
  selectedPipeline: string;
  /**
   * The worker the Playground last dispatched to, when it has one.
   *
   * Preferred over asking the orchestrator again, because dispatch returns
   * the *least loaded* worker: a session in trouble is holding a connection,
   * which makes its own worker the least likely to be chosen. Re-dispatching
   * would quietly probe a different -- probably healthy -- worker than the one
   * being complained about, and report that it is fine.
   */
  preferredWorkerId?: string | null;
  /** The Playground's browser-processing setting, so a recording here is
   *  captured the way a session would capture it. */
  browserAudioProcessing?: boolean;
  onClose: () => void;
}

type StageState = {
  running: boolean;
  assessment?: Assessment;
  detail?: string;
  /** What the stage produced, verbatim. */
  output?: string;
  error?: string;
};

type Source = "fixture" | "mic";

/** The wire rate a cascade session negotiates, and what the probes expect. */
const RECORD_RATE = 16_000;

/** A probe is for a phrase. The worker refuses bodies much past this. */
const MAX_RECORD_MS = 15_000;

const STAGES: { kind: ProbeKind; label: string; blurb: string }[] = [
  { kind: "vad", label: "VAD", blurb: "segments audio; the only stage that discards it" },
  { kind: "stt", label: "STT", blurb: "turns the whole input into words" },
  { kind: "llm", label: "LLM", blurb: "answers the words" },
  { kind: "tts", label: "TTS", blurb: "speaks the answer" },
];

function errorMessage(e: unknown): string {
  return axios.isAxiosError(e)
    ? (e.response?.data?.detail?.error?.message ?? e.message)
    : String(e);
}

export default function ComponentCheckDialog({
  apiBase, selectedPipeline, preferredWorkerId, browserAudioProcessing = true, onClose,
}: ComponentCheckDialogProps) {
  const { isDark } = useTheme();

  const [workerId, setWorkerId] = useState<string | null>(null);
  /** How the worker above was chosen, so the reader can tell whether it is
   *  the one their session is on. */
  const [workerSource, setWorkerSource] = useState<"session" | "dispatch" | null>(null);
  const [fixtures, setFixtures] = useState<Fixture[]>([]);
  const [fixture, setFixture] = useState("");
  const [source, setSource] = useState<Source>("fixture");
  const [prompt, setPrompt] = useState("Hello, how are you?");
  const [ttsText, setTtsText] = useState("The quick brown fox jumps over the lazy dog.");
  const [stages, setStages] = useState<Record<ProbeKind, StageState>>({
    vad: { running: false }, stt: { running: false },
    llm: { running: false }, tts: { running: false },
  });
  const [setupError, setSetupError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  // ── Microphone recording ──
  const [recording, setRecording] = useState(false);
  const [recordedWav, setRecordedWav] = useState<ArrayBuffer | null>(null);
  const [recordedMs, setRecordedMs] = useState(0);
  const [recordedUrl, setRecordedUrl] = useState<string | null>(null);
  const [recordError, setRecordError] = useState<string | null>(null);
  /** What the speaker says they said. Optional, and the only thing that lets
   *  the first-word check run on a recording. */
  const [saidText, setSaidText] = useState("");
  const captureRef = useRef<AudioCaptureService | null>(null);
  const blocksRef = useRef<Int16Array[]>([]);
  const stopTimerRef = useRef<number | null>(null);

  const [ttsAudioUrl, setTtsAudioUrl] = useState<string | null>(null);

  // Object URLs hold their blob until revoked; replacing one without revoking
  // the last leaks every take for the life of the page.
  useEffect(() => () => { if (recordedUrl) URL.revokeObjectURL(recordedUrl); }, [recordedUrl]);
  useEffect(() => () => { if (ttsAudioUrl) URL.revokeObjectURL(ttsAudioUrl); }, [ttsAudioUrl]);

  // Resolve a worker without opening a session: the check is wanted most when
  // sessions are not working, so it must not depend on one.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        let id = preferredWorkerId ?? null;
        let src: "session" | "dispatch" = "session";
        if (!id) {
          const params: Record<string, string> = {};
          if (selectedPipeline) params.pipeline_name = selectedPipeline;
          const res = await axios.get(`${apiBase}/api/v1/dispatch`, { params });
          id = res.data?.data?.worker_id ?? null;
          src = "dispatch";
        }
        if (cancelled) return;
        if (typeof id !== "string" || !id) {
          setSetupError("The orchestrator returned no worker for this pipeline.");
          return;
        }
        setWorkerId(id);
        setWorkerSource(src);
        const fx = await axios.get(`${apiBase}/api/v1/workers/${encodeURIComponent(id)}/probe/fixtures`);
        if (cancelled) return;
        const list: Fixture[] = fx.data?.data?.fixtures ?? [];
        setFixtures(list);
        setFixture(list.find((f) => f.name === "hello-how-are-you")?.name ?? list[0]?.name ?? "");
      } catch (e) {
        if (!cancelled) setSetupError(errorMessage(e));
      }
    })();
    return () => { cancelled = true; };
  }, [apiBase, selectedPipeline, preferredWorkerId]);

  const chosen = useMemo(
    () => fixtures.find((f) => f.name === fixture),
    [fixtures, fixture],
  );

  /** What the audio is supposed to say, if anyone knows. Undefined means
   *  unknown, which is different from "" (known to hold no speech). */
  const expectedText: string | undefined =
    source === "fixture"
      ? chosen?.text
      : saidText.trim() ? saidText.trim() : undefined;
  /** Whether the input is expected to contain speech at all. A recording is
   *  assumed to: nobody records silence to test a recogniser on purpose. */
  const expectsSpeech = source === "mic" ? true : (chosen?.text ?? "").trim() !== "";

  const stopRecording = useCallback(() => {
    if (stopTimerRef.current !== null) {
      window.clearTimeout(stopTimerRef.current);
      stopTimerRef.current = null;
    }
    const cap = captureRef.current;
    captureRef.current = null;
    cap?.stop();
    setRecording(false);

    const samples = concatBlocks(blocksRef.current);
    blocksRef.current = [];
    if (samples.length === 0) {
      setRecordError("Nothing was captured. Check the microphone with the microphone check.");
      return;
    }
    const wav = encodeWav(samples, RECORD_RATE);
    setRecordedWav(wav);
    setRecordedMs(Math.round((samples.length / RECORD_RATE) * 1000));
    setRecordedUrl(URL.createObjectURL(new Blob([wav], { type: "audio/wav" })));
  }, []);

  const startRecording = useCallback(async () => {
    setRecordError(null);
    setRecordedWav(null);
    setRecordedUrl(null);
    setRecordedMs(0);
    blocksRef.current = [];
    const cap = new AudioCaptureService();
    // Copied, not kept: the capture path may reuse a block's buffer.
    cap.onSamples = (s) => { blocksRef.current.push(new Int16Array(s)); };
    try {
      await cap.start(RECORD_RATE, {
        echoCancellation: browserAudioProcessing,
        noiseSuppression: browserAudioProcessing,
        autoGainControl: browserAudioProcessing,
      });
    } catch (e) {
      setRecordError(`The browser refused the microphone: ${String(e)}`);
      return;
    }
    captureRef.current = cap;
    setRecording(true);
    stopTimerRef.current = window.setTimeout(stopRecording, MAX_RECORD_MS);
  }, [browserAudioProcessing, stopRecording]);

  // Never leave the microphone open behind a closed dialog: a second holder
  // of the device is itself a cause of silent audio in the next session.
  useEffect(() => () => {
    if (stopTimerRef.current !== null) window.clearTimeout(stopTimerRef.current);
    captureRef.current?.stop();
  }, []);

  const setStage = useCallback((kind: ProbeKind, patch: Partial<StageState>) => {
    setStages((prev) => ({ ...prev, [kind]: { ...prev[kind], ...patch } }));
  }, []);

  const audioReady = source === "fixture" ? !!fixture : !!recordedWav;

  const run = useCallback(async (kind: ProbeKind): Promise<void> => {
    if (!workerId) return;
    setStage(kind, {
      running: true, error: undefined, assessment: undefined,
      detail: undefined, output: undefined,
    });
    if (kind === "tts") setTtsAudioUrl(null);
    const url = `${apiBase}/api/v1/workers/${encodeURIComponent(workerId)}/probe/${encodeURIComponent(kind)}`;
    try {
      let data;
      if (kind === "vad" || kind === "stt") {
        if (source === "fixture") {
          data = (await axios.post(url, null, { params: { fixture } })).data?.data;
        } else {
          if (!recordedWav) throw new Error("record something first");
          data = (await axios.post(url, recordedWav, {
            headers: { "Content-Type": "audio/wav" },
          })).data?.data;
        }
      } else {
        const text = kind === "llm" ? prompt : ttsText;
        data = (await axios.post(url, { text })).data?.data;
      }

      let assessment: Assessment;
      let detail = "";
      let output: string | undefined;
      if (kind === "vad") {
        const r = data as VadResult;
        assessment = assessVad(r, expectsSpeech);
        detail =
          `${r.backend} · pre-roll ${r.pre_roll_ms} ms · forwarded ` +
          `${r.total_forwarded_ms} ms of ${r.input_duration_ms} ms`;
      } else if (kind === "stt") {
        const r = data as SttResult;
        // The probe transcribes the whole input, so what it misses is its own.
        assessment = assessStt(r, expectedText, { inputWasSegmented: false });
        output = r.transcript;
        detail = `${r.provider} · ${Math.round(r.processing_ms)} ms · rtfx ${r.rtfx.toFixed(1)}`;
      } else if (kind === "tts") {
        const r = data as TtsResult;
        assessment = assessTts(r);
        detail = `${r.provider} · ${r.sample_rate} Hz`;
        // Fetched as a blob rather than pointed at by URL: an audio element
        // cannot attach the console's credentials, and a failure here should
        // say so rather than leave a silent player.
        if (r.audio_file) {
          try {
            const res = await axios.get(
              `${apiBase}/api/v1/workers/${encodeURIComponent(workerId)}/probe/audio/${encodeURIComponent(r.audio_file)}`,
              { responseType: "blob" },
            );
            setTtsAudioUrl(URL.createObjectURL(res.data as Blob));
          } catch (e) {
            detail += ` · audio unavailable: ${errorMessage(e)}`;
          }
        }
      } else {
        const r = data as LlmResult;
        assessment = assessLlm(r);
        output = r.reply;
        detail = `${Math.round(r.total_ms)} ms · ${r.chars} characters`;
      }
      setStage(kind, { running: false, assessment, detail, output });
    } catch (e) {
      setStage(kind, { running: false, error: errorMessage(e) });
    }
  }, [apiBase, workerId, source, fixture, recordedWav, prompt, ttsText,
      expectedText, expectsSpeech, setStage]);

  const runAll = useCallback(async () => {
    // Sequentially: the worker runs one probe at a time and refuses the rest,
    // so firing them together would fail three of four on purpose.
    for (const s of STAGES) await run(s.kind);
  }, [run]);

  const copyReport = useCallback(async () => {
    const text = formatReport({
      pipeline: selectedPipeline,
      workerId,
      workerSource,
      fixture: source === "fixture"
        ? fixture
        : `microphone recording, ${(recordedMs / 1000).toFixed(1)} s` +
          (browserAudioProcessing ? ", browser processing on" : ", browser processing off"),
      fixtureText: expectedText ?? "",
      recorded: source === "mic",
      prompt,
      ttsText,
      generatedAt: new Date(),
      stages: STAGES.map((s) => ({
        kind: s.kind,
        label: s.label,
        assessment: stages[s.kind].assessment,
        detail: stages[s.kind].detail,
        output: stages[s.kind].output,
        error: stages[s.kind].error,
      })),
    });
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // The clipboard API needs a secure context and permission, and refuses
      // often enough that silently doing nothing would look like a dead
      // button. Fall back to a selection the person can copy by hand.
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      try {
        document.execCommand("copy");
      } finally {
        document.body.removeChild(ta);
      }
    }
    setCopied(true);
    window.setTimeout(() => setCopied(false), 2000);
  }, [selectedPipeline, workerId, workerSource, source, fixture, recordedMs,
      browserAudioProcessing, expectedText, prompt, ttsText, stages]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const summary = useMemo(
    () => summariseRun(
      STAGES.flatMap((s) =>
        stages[s.kind].assessment
          ? [{ kind: s.kind, assessment: stages[s.kind].assessment! }]
          : [],
      ),
    ),
    [stages],
  );

  const card = isDark ? "bg-slate-800 border-slate-700" : "bg-white border-gray-200";
  const text = isDark ? "text-slate-200" : "text-gray-800";
  const muted = isDark ? "text-slate-400" : "text-gray-500";
  const field = isDark
    ? "bg-slate-900 border-slate-700 text-slate-200"
    : "bg-white border-gray-300 text-gray-800";
  const inset = isDark ? "border-slate-700 bg-slate-900" : "border-gray-200 bg-gray-50";
  const busy = recording || STAGES.some((s) => stages[s.kind].running);
  const ran = STAGES.some((s) => stages[s.kind].assessment || stages[s.kind].error);

  /** Audio stages need audio; text stages only need a worker. */
  const canRun = (kind: ProbeKind) =>
    !busy && !!workerId && (kind === "vad" || kind === "stt" ? audioReady : true);

  const tab = (value: Source, label: string) => (
    <button
      type="button"
      onClick={() => setSource(value)}
      disabled={busy}
      className={`text-xs px-3 py-1.5 rounded-md border ${
        source === value
          ? "bg-blue-600 border-blue-600 text-white"
          : `${field} hover:border-blue-500`}`}
    >
      {label}
    </button>
  );

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/60 p-4 sm:p-8 overflow-y-auto"
      onMouseDown={(e) => { if (e.target === e.currentTarget) onClose(); }}
      role="dialog" aria-modal="true" aria-label="Component check"
    >
      <div className={`w-full max-w-3xl rounded-xl border shadow-xl flex flex-col gap-4 p-5 ${card}`}>
        <header className="flex items-start justify-between gap-4">
          <div>
            <h2 className={`text-lg font-semibold ${text}`}>Component check</h2>
            <p className={`text-xs mt-1 ${muted}`}>
              Runs one stage at a time against a known input. Stages are listed in the
              order audio flows through them.
            </p>
            <p className={`text-xs mt-1 ${muted}`}>
              Probing{" "}
              <span className={`font-mono ${text}`}>{workerId ?? "…"}</span>
              {workerSource === "session"
                ? " — the worker this Playground dispatched to."
                : workerSource === "dispatch"
                  ? ` — chosen by the orchestrator for ${selectedPipeline || "any pipeline"}, ` +
                    "which picks the least loaded worker and so may not be the one a " +
                    "troubled session is on."
                  : ""}
            </p>
          </div>
          <button onClick={onClose} aria-label="Close component check"
            className={isDark ? "text-slate-400 hover:text-white" : "text-gray-400 hover:text-gray-900"}>
            <X size={18} />
          </button>
        </header>

        {setupError && (
          <p className="text-xs rounded-md px-3 py-2 bg-red-500/10 text-red-500 border border-red-500/30">
            {setupError}
          </p>
        )}

        {/* ── Audio input for VAD and STT ── */}
        <section className={`rounded-md border p-3 flex flex-col gap-3 ${inset}`}>
          <div className="flex items-center gap-2">
            <span className={`text-xs font-medium ${muted} mr-1`}>Audio for VAD and STT</span>
            {tab("fixture", "Fixture")}
            {tab("mic", "My microphone")}
          </div>

          {source === "fixture" ? (
            <>
              <select className={`border rounded-md px-2 py-1.5 text-sm ${field}`}
                value={fixture} disabled={busy || !fixtures.length}
                onChange={(e) => setFixture(e.target.value)}>
                {fixtures.length === 0 && <option>{setupError ? "unavailable" : "loading…"}</option>}
                {fixtures.map((f) => (
                  <option key={f.name} value={f.name}>
                    {f.name} — {f.text ? `"${f.text}"` : "(no speech)"}
                  </option>
                ))}
              </select>
              {chosen?.note && <p className={`text-xs italic ${muted}`}>{chosen.note}</p>}
            </>
          ) : (
            <>
              <div className="flex flex-wrap items-center gap-3">
                <button
                  type="button"
                  onClick={() => (recording ? stopRecording() : void startRecording())}
                  disabled={!recording && busy}
                  className={`inline-flex items-center gap-2 rounded-md px-3 py-1.5 text-sm font-medium text-white ${
                    recording ? "bg-red-600 hover:bg-red-500" : "bg-blue-600 hover:bg-blue-500"}`}
                >
                  {recording ? <Square size={14} /> : <Mic size={14} />}
                  {recording ? "Stop" : recordedWav ? "Record again" : "Record"}
                </button>
                {recording && (
                  <span className={`text-xs ${muted}`}>
                    Recording — speak, then stop (stops itself after {MAX_RECORD_MS / 1000} s)
                  </span>
                )}
                {recordedUrl && !recording && (
                  <>
                    <span className={`text-xs font-mono ${muted}`}>
                      {(recordedMs / 1000).toFixed(1)} s
                    </span>
                    {/* Hearing the take is the fastest check that capture
                        worked before blaming anything downstream of it. */}
                    <audio src={recordedUrl} controls className="h-8" />
                  </>
                )}
              </div>
              <p className={`text-xs ${muted}`}>
                Recorded at {RECORD_RATE / 1000} kHz through the same capture path a
                session uses, browser processing{" "}
                <strong>{browserAudioProcessing ? "on" : "off"}</strong> to match your
                Playground setting.
              </p>
              <label className="flex flex-col gap-1.5">
                <span className={`text-xs font-medium ${muted}`}>
                  What you said (optional — enables the first-word check)
                </span>
                <input className={`border rounded-md px-2 py-1.5 text-sm ${field}`}
                  value={saidText} disabled={busy} placeholder="Hello, how are you?"
                  onChange={(e) => setSaidText(e.target.value)} />
              </label>
              {recordError && <p className="text-xs text-red-500">{recordError}</p>}
            </>
          )}
        </section>

        {/* ── Text inputs for LLM and TTS ── */}
        <div className="grid gap-3 sm:grid-cols-2">
          <label className="flex flex-col gap-1.5">
            <span className={`text-xs font-medium ${muted}`}>Prompt for the model</span>
            <input className={`border rounded-md px-2 py-1.5 text-sm ${field}`}
              value={prompt} disabled={busy} onChange={(e) => setPrompt(e.target.value)} />
          </label>
          <label className="flex flex-col gap-1.5">
            <span className={`text-xs font-medium ${muted}`}>Text to speak</span>
            <input className={`border rounded-md px-2 py-1.5 text-sm ${field}`}
              value={ttsText} disabled={busy} onChange={(e) => setTtsText(e.target.value)} />
          </label>
        </div>

        <div className="flex justify-end">
          <button onClick={() => void runAll()} disabled={busy || !workerId || !audioReady || !!setupError}
            title={!audioReady ? "Record something first, or pick a fixture" : undefined}
            className={`inline-flex items-center gap-2 rounded-md px-3 py-1.5 text-sm font-medium text-white ${
              busy || !workerId || !audioReady ? "bg-slate-600" : "bg-blue-600 hover:bg-blue-500"}`}>
            <Play size={14} /> Run all
          </button>
        </div>

        <div className="flex flex-col gap-2">
          {STAGES.map((s) => {
            const st = stages[s.kind];
            return (
              <div key={s.kind} className={`rounded-md border p-3 flex gap-3 items-start ${inset}`}>
                <StatusIcon status={st.error ? "critical" : st.assessment?.status} isDark={isDark} />
                <div className="min-w-0 grow">
                  <div className="flex items-baseline gap-2">
                    <span className={`text-sm font-semibold ${text}`}>{s.label}</span>
                    <span className={`text-xs ${muted}`}>{s.blurb}</span>
                  </div>
                  {st.running && <p className={`text-xs mt-1 ${muted}`}>running…</p>}
                  {st.error && <p className="text-xs mt-1 text-red-500">{st.error}</p>}
                  {st.output !== undefined && (
                    // The verbatim output, set apart from the verdict: a
                    // judgement about a transcript is no substitute for
                    // reading it.
                    <p className={`text-sm mt-1.5 rounded px-2 py-1 font-mono break-words ${
                      isDark ? "bg-slate-800 text-slate-100" : "bg-white text-gray-900"}`}>
                      {st.output.trim() ? st.output : <em className={muted}>(no words)</em>}
                    </p>
                  )}
                  {s.kind === "tts" && ttsAudioUrl && (
                    <audio src={ttsAudioUrl} controls className="mt-1.5 h-8 w-full" />
                  )}
                  {st.assessment && (
                    <>
                      <p className={`text-xs mt-1 ${text}`}>{st.assessment.summary}</p>
                      {st.assessment.hint && (
                        <p className={`text-xs mt-0.5 ${muted}`}>{st.assessment.hint}</p>
                      )}
                      {st.detail && (
                        <p className={`text-[11px] mt-1 font-mono ${muted} truncate`}>{st.detail}</p>
                      )}
                    </>
                  )}
                </div>
                <button onClick={() => void run(s.kind)} disabled={!canRun(s.kind)}
                  className={`shrink-0 text-xs rounded-md border px-2 py-1 ${field} ${
                    canRun(s.kind) ? "hover:border-blue-500" : "opacity-50"}`}>
                  Run
                </button>
              </div>
            );
          })}
        </div>

        <div className={`rounded-md p-3 ${isDark ? "bg-slate-900" : "bg-gray-50"}`}>
          <div className="flex gap-3">
            <StatusIcon status={summary.status} isDark={isDark} />
            <p className={`text-sm font-semibold ${toneOf(summary.status, isDark)}`}>
              {summary.headline}
            </p>
          </div>
          <p className={`text-xs mt-1 ${muted}`}>
            Each stage is measured on its own — the recogniser gets the whole input, not
            what the VAD passed on — so each verdict belongs to its own stage. In a live
            session, speech the VAD drops never reaches the recogniser at all.
          </p>
          <button
            onClick={() => void copyReport()}
            disabled={!ran}
            className={`mt-2 inline-flex items-center gap-1.5 text-xs rounded-md border px-2 py-1 ${field} ${
              ran ? "hover:border-blue-500" : "opacity-50"}`}
          >
            {copied ? <Check size={13} /> : <Copy size={13} />}
            {copied ? "Copied" : "Copy report"}
          </button>
        </div>
      </div>
    </div>
  );
}

function toneOf(status: Status | undefined, isDark: boolean): string {
  switch (status) {
    case "good": return "text-emerald-500";
    case "critical": return "text-red-500";
    case "warning": return "text-amber-500";
    default: return isDark ? "text-slate-400" : "text-gray-400";
  }
}

/** Status carries an icon beside its sentence, never colour alone. */
function StatusIcon({ status, isDark }: { status?: Status; isDark: boolean }) {
  const Icon =
    status === "good" ? CheckCircle2 :
    status === "critical" ? XCircle :
    status === "warning" ? AlertTriangle : Circle;
  return <Icon size={18} className={`${toneOf(status, isDark)} shrink-0 mt-0.5`} aria-hidden />;
}

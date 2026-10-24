import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, CheckCircle2, Circle, Mic, Square, X, XCircle } from "lucide-react";
import { useTheme } from "../../../contexts/ThemeContext";
import {
  SPEECH_FLOOR_RMS, classify, compare, summarise,
  type Block, type Comparison, type Summary, type Take, type Verdict,
} from "./micCheck.logic";

/**
 * A microphone check that shares no code with the voice pipeline.
 *
 * That is the point of it, not an oversight. When a session transcribes
 * nothing, the question is whether the microphone is handing over usable
 * audio at all -- and an answer produced by the same capture path that is
 * under suspicion cannot settle it. This dialog talks to `getUserMedia` and
 * the Web Audio API directly: if it shows speech and the Playground does not,
 * the fault is ours; if it shows nothing, no amount of pipeline work will help.
 *
 * It lives inside the Playground rather than off a menu of its own because
 * that is where the question gets asked -- next to the microphone-processing
 * toggle, at the moment a session is transcribing nothing.
 */

/** Analysis block size. Small enough to see a syllable, large enough that the
 *  UI is not re-rendering per audio quantum. */
const BLOCK_SIZE = 2048;

/** How much history the envelope shows. */
const ENVELOPE_MS = 12000;

interface TrackFacts {
  label: string;
  readyState: string;
  muted: boolean;
  enabled: boolean;
  sampleRate: number | string;
  channelCount: number | string;
  echoCancellation: boolean | string;
  noiseSuppression: boolean | string;
  autoGainControl: boolean | string;
  contextRate: number | string;
}

export interface MicCheckDialogProps {
  onClose: () => void;
  /** Whether a session currently holds the microphone. The check opens its
   *  own `getUserMedia` stream, and two consumers of one device is exactly
   *  the condition that makes a track go live-but-silent — so say so rather
   *  than letting the check produce a result that means nothing. */
  sessionHoldsMic?: boolean;
}

export default function MicCheckDialog({ onClose, sessionHoldsMic = false }: MicCheckDialogProps) {
  const { theme } = useTheme();
  const isDark = theme === "dark";

  const [devices, setDevices] = useState<MediaDeviceInfo[]>([]);
  const [deviceId, setDeviceId] = useState("");
  const [processing, setProcessing] = useState(true);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [level, setLevel] = useState(0);
  const [summary, setSummary] = useState<Summary>(() => summarise([]));
  const [verdict, setVerdict] = useState<Verdict | null>(null);
  const [facts, setFacts] = useState<TrackFacts | null>(null);
  const [envelope, setEnvelope] = useState<Block[]>([]);
  const [takes, setTakes] = useState<{ on?: Take; off?: Take }>({});
  const [comparison, setComparison] = useState<Comparison | null>(null);

  const ctxRef = useRef<AudioContext | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const nodeRef = useRef<ScriptProcessorNode | null>(null);
  const srcRef = useRef<MediaStreamAudioSourceNode | null>(null);
  const blocksRef = useRef<Block[]>([]);
  const startedAt = useRef(0);

  const stop = useCallback(() => {
    if (nodeRef.current) {
      nodeRef.current.onaudioprocess = null;
      nodeRef.current.disconnect();
    }
    srcRef.current?.disconnect();
    streamRef.current?.getTracks().forEach((t) => t.stop());
    void ctxRef.current?.close();
    nodeRef.current = null;
    srcRef.current = null;
    streamRef.current = null;
    ctxRef.current = null;
    setRunning(false);
    setLevel(0);
  }, []);

  useEffect(() => stop, [stop]);

  const refreshDevices = useCallback(async () => {
    try {
      const all = await navigator.mediaDevices.enumerateDevices();
      setDevices(all.filter((d) => d.kind === "audioinput"));
    } catch {
      // Listing needs permission on some browsers; the picker simply stays
      // on "default", which still works.
    }
  }, []);

  // No listing on mount: until the browser has granted microphone access,
  // `enumerateDevices` returns entries whose labels are empty strings, so the
  // picker would offer a column of anonymous ids. The list is filled after the
  // first successful `getUserMedia` instead, and kept current from there by
  // the device-change event.
  useEffect(() => {
    const md = navigator.mediaDevices;
    if (!md?.addEventListener) return;
    const onChange = () => void refreshDevices();
    md.addEventListener("devicechange", onChange);
    return () => md.removeEventListener("devicechange", onChange);
  }, [refreshDevices]);

  const start = useCallback(async () => {
    stop();
    setError(null);
    setVerdict(null);
    setEnvelope([]);
    blocksRef.current = [];
    setSummary(summarise([]));

    const audio: MediaTrackConstraints = {
      channelCount: 1,
      echoCancellation: processing,
      noiseSuppression: processing,
      autoGainControl: processing,
    };
    if (deviceId) audio.deviceId = { exact: deviceId };

    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio });
    } catch (e) {
      const err = e as DOMException;
      setError(`${err.name}: ${err.message}`);
      return;
    }
    streamRef.current = stream;
    void refreshDevices();

    // No sampleRate on the context: the device runs at its own rate and
    // nothing resamples, so what is measured is what the microphone gave.
    const ctx = new AudioContext();
    ctxRef.current = ctx;
    if (ctx.state === "suspended") await ctx.resume();

    const track = stream.getAudioTracks()[0];
    const s = track.getSettings();
    setFacts({
      label: track.label || "(unlabelled device)",
      readyState: track.readyState,
      muted: track.muted,
      enabled: track.enabled,
      sampleRate: s.sampleRate ?? "not reported",
      channelCount: s.channelCount ?? "not reported",
      echoCancellation: s.echoCancellation ?? "not reported",
      noiseSuppression: s.noiseSuppression ?? "not reported",
      autoGainControl: s.autoGainControl ?? "not reported",
      contextRate: ctx.sampleRate,
    });

    const src = ctx.createMediaStreamSource(stream);
    srcRef.current = src;
    const node = ctx.createScriptProcessor(BLOCK_SIZE, 1, 1);
    nodeRef.current = node;
    startedAt.current = performance.now();

    node.onaudioprocess = (e) => {
      const ch = e.inputBuffer.getChannelData(0);
      let sum = 0;
      let peak = 0;
      for (let i = 0; i < ch.length; i++) {
        const a = Math.abs(ch[i]);
        if (a > peak) peak = a;
        sum += ch[i] * ch[i];
      }
      const rms = Math.sqrt(sum / ch.length);
      const t = performance.now() - startedAt.current;
      blocksRef.current.push({ t, rms, peak });
      setLevel(rms);
      const kept = blocksRef.current.filter((b) => b.t > t - ENVELOPE_MS);
      setEnvelope(kept);
      const sum2 = summarise(blocksRef.current);
      setSummary(sum2);
      setVerdict(classify(sum2, t));
    };

    src.connect(node);
    // Through a silent gain node: the processor only runs while connected to
    // the destination, and routing the microphone to the speakers would feed
    // back and give echo cancellation something real to chew on.
    const silent = ctx.createGain();
    silent.gain.value = 0;
    node.connect(silent);
    silent.connect(ctx.destination);
    setRunning(true);
  }, [deviceId, processing, refreshDevices, stop]);

  /** Keep the current run as the take for whichever processing mode it used. */
  const keepTake = useCallback(() => {
    const take: Take = {
      label: processing ? "processing on" : "processing off",
      processing,
      summary: summarise(blocksRef.current),
    };
    setTakes((prev) => {
      const next = processing ? { ...prev, on: take } : { ...prev, off: take };
      setComparison(next.on && next.off ? compare(next.on, next.off) : null);
      return next;
    });
  }, [processing]);

  // Closing must release the device. A check that keeps the microphone after
  // it is dismissed becomes the second consumer that makes the next session's
  // track silent -- the very fault it exists to diagnose.
  const close = useCallback(() => {
    stop();
    onClose();
  }, [stop, onClose]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [close]);

  // ── styling, matching the rest of the console ──
  const card = isDark ? "bg-slate-800 border-slate-700" : "bg-white border-gray-200";
  const text = isDark ? "text-slate-200" : "text-gray-800";
  const muted = isDark ? "text-slate-400" : "text-gray-500";
  const field = isDark
    ? "bg-slate-900 border-slate-700 text-slate-200"
    : "bg-white border-gray-300 text-gray-800";

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/60 p-4 sm:p-8 overflow-y-auto"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
      role="dialog"
      aria-modal="true"
      aria-label="Microphone check"
    >
      <div className={`w-full max-w-3xl rounded-xl border shadow-xl flex flex-col gap-4 p-5 ${card}`}>
        <header className="flex items-start justify-between gap-4">
          <div>
            <h2 className={`text-lg font-semibold ${text}`}>Microphone check</h2>
            <p className={`text-xs mt-1 ${muted}`}>
              Plain Web Audio, sharing no code with the voice pipeline. If speech
              shows up here but a session transcribes nothing, the fault is in the
              pipeline. If nothing shows up here, no pipeline change will fix it.
            </p>
          </div>
          <button
            onClick={close}
            aria-label="Close microphone check"
            className={isDark ? "text-slate-400 hover:text-white" : "text-gray-400 hover:text-gray-900"}
          >
            <X size={18} />
          </button>
        </header>

        {sessionHoldsMic && (
          <p className="text-xs rounded-md px-3 py-2 bg-amber-500/10 text-amber-500 border border-amber-500/30">
            A session is holding the microphone. Two consumers of one device is
            itself a cause of silent audio, so stop the session before trusting
            anything measured here.
          </p>
        )}

      <section className={`border rounded-lg p-4 flex flex-col gap-3 ${card}`}>
        <div className="flex flex-wrap items-end gap-3">
          <label className="flex flex-col gap-1.5 grow min-w-[16rem]">
            <span className={`text-xs font-medium ${muted}`}>Input device</span>
            <select
              className={`border rounded-md px-2 py-1.5 text-sm ${field}`}
              value={deviceId}
              disabled={running}
              onChange={(e) => setDeviceId(e.target.value)}
            >
              <option value="">Default device</option>
              {devices.map((d) => (
                <option key={d.deviceId} value={d.deviceId}>
                  {d.label || `Microphone ${d.deviceId.slice(0, 8)}`}
                </option>
              ))}
            </select>
          </label>

          <label className={`flex items-center gap-2 text-sm ${text} pb-1.5`}>
            <input
              type="checkbox"
              checked={processing}
              disabled={running}
              onChange={(e) => setProcessing(e.target.checked)}
            />
            Browser processing
            <span className={`text-xs ${muted}`}>
              (echo cancellation, noise suppression, auto gain)
            </span>
          </label>

          <button
            onClick={running ? stop : () => void start()}
            className={`inline-flex items-center gap-2 rounded-md px-3 py-1.5 text-sm font-medium text-white ${
              running ? "bg-slate-600 hover:bg-slate-500" : "bg-blue-600 hover:bg-blue-500"
            }`}
          >
            {running ? <Square size={14} /> : <Mic size={14} />}
            {running ? "Stop" : "Start"}
          </button>
        </div>

        {error && (
          <p className="text-sm text-red-500">
            The browser refused access — {error}
          </p>
        )}

        <Meter level={level} isDark={isDark} />
        <Envelope blocks={envelope} isDark={isDark} />

        <dl className={`grid grid-cols-2 sm:grid-cols-4 gap-x-4 gap-y-1 text-xs font-mono ${muted}`}>
          <Stat label="blocks" value={String(summary.blocks)} />
          <Stat label="peak" value={summary.peak.toFixed(4)} />
          <Stat label="speech" value={`${(summary.activeMs / 1000).toFixed(2)} s`} />
          <Stat label="longest run" value={`${(summary.longestRunMs / 1000).toFixed(2)} s`} />
        </dl>

        {verdict && <VerdictRow verdict={verdict} isDark={isDark} />}
      </section>

      <section className={`border rounded-lg p-4 flex flex-col gap-3 ${card}`}>
        <div>
          <h2 className={`text-sm font-semibold ${text}`}>
            Is browser processing eating your speech?
          </h2>
          <p className={`text-xs mt-1 ${muted}`}>
            Say the same sentence twice — once with the box above ticked, once with
            it clear — and keep each take. Echo cancellation and noise suppression
            run before any application sees the audio, so saying the same thing
            twice is the only way to measure what they removed.
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-3">
          <button
            onClick={keepTake}
            disabled={summary.blocks === 0}
            className={`rounded-md px-3 py-1.5 text-sm font-medium border ${field} ${
              summary.blocks === 0 ? "opacity-50" : "hover:border-blue-500"
            }`}
          >
            Keep this take as “processing {processing ? "on" : "off"}”
          </button>
          <TakeChip take={takes.on} label="processing on" isDark={isDark} />
          <TakeChip take={takes.off} label="processing off" isDark={isDark} />
        </div>

        {comparison && (
          <VerdictRow
            verdict={{
              kind: "ok",
              status: comparison.status,
              title: comparison.title,
              detail: comparison.detail,
            }}
            isDark={isDark}
          />
        )}
      </section>

      {facts && (
        <section className={`border rounded-lg p-4 ${card}`}>
          <h2 className={`text-sm font-semibold mb-2 ${text}`}>What the track reports</h2>
          <dl className={`grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-1 text-xs font-mono ${muted}`}>
            {Object.entries(facts).map(([k, v]) => (
              <Stat key={k} label={k} value={String(v)} />
            ))}
          </dl>
        </section>
        )}
      </div>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-2 border-b border-dashed border-current/10 py-0.5">
      <dt>{label}</dt>
      <dd className="font-semibold">{value}</dd>
    </div>
  );
}

function Meter({ level, isDark }: { level: number; isDark: boolean }) {
  // Scaled so ordinary speech (rms ~0.08) lands mid-meter rather than in the
  // first tenth, which would make a quiet-but-working mic look dead.
  const pct = Math.min(100, level * 300);
  return (
    <div className={`h-6 rounded overflow-hidden ${isDark ? "bg-slate-900" : "bg-gray-100"}`}>
      <div
        className="h-full bg-emerald-500 transition-[width] duration-75"
        style={{ width: `${pct}%` }}
        role="meter"
        aria-label="input level"
        aria-valuenow={Math.round(pct)}
      />
    </div>
  );
}

/**
 * The loudness envelope over time.
 *
 * One series, so no legend; the heading names it. The dashed rule is the floor
 * that separates speech from room tone, which is what makes a gated phrase
 * legible at a glance: speech with the middle removed shows as spikes over an
 * empty baseline rather than a continuous ridge.
 */
function Envelope({ blocks, isDark }: { blocks: Block[]; isDark: boolean }) {
  const W = 1000;
  const H = 90;
  if (blocks.length < 2) {
    return (
      <div
        className={`h-[90px] rounded flex items-center justify-center text-xs ${
          isDark ? "bg-slate-900 text-slate-500" : "bg-gray-50 text-gray-400"
        }`}
      >
        Loudness over time appears here once audio arrives
      </div>
    );
  }
  const t0 = blocks[0].t;
  const span = Math.max(1, blocks[blocks.length - 1].t - t0);
  // Fixed ceiling rather than auto-scaling: an auto-scaled envelope makes
  // room tone look like speech when nothing louder ever arrives.
  const ceil = 0.4;
  const y = (rms: number) => H - Math.min(1, rms / ceil) * (H - 4) - 2;
  const pts = blocks.map((b) => `${((b.t - t0) / span) * W},${y(b.rms)}`).join(" ");
  const floorY = y(SPEECH_FLOOR_RMS);

  return (
    <svg
      viewBox={`0 0 ${W} ${H}`}
      preserveAspectRatio="none"
      className={`w-full h-[90px] rounded ${isDark ? "bg-slate-900" : "bg-gray-50"}`}
      role="img"
      aria-label="Microphone loudness over time"
    >
      <line
        x1="0" x2={W} y1={floorY} y2={floorY}
        stroke="currentColor" strokeDasharray="4 4"
        className={isDark ? "text-slate-600" : "text-gray-300"}
      />
      <polyline points={pts} fill="none" stroke="#10b981" strokeWidth="2"
        vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

function TakeChip({ take, label, isDark }: { take?: Take; label: string; isDark: boolean }) {
  const base = "text-xs font-mono rounded px-2 py-1 border";
  if (!take) {
    return (
      <span className={`${base} ${isDark ? "border-slate-700 text-slate-500" : "border-gray-200 text-gray-400"}`}>
        {label}: not recorded
      </span>
    );
  }
  return (
    <span className={`${base} ${isDark ? "border-slate-600 text-slate-300" : "border-gray-300 text-gray-600"}`}>
      {label}: {(take.summary.activeMs / 1000).toFixed(2)} s speech, peak{" "}
      {take.summary.peak.toFixed(3)}
    </span>
  );
}

/** Status carries an icon and a sentence, never colour alone. */
function VerdictRow({ verdict, isDark }: { verdict: Verdict | (Omit<Verdict, "kind"> & { kind?: string }); isDark: boolean }) {
  const { status, title, detail } = verdict;
  const Icon =
    status === "good" ? CheckCircle2 :
    status === "critical" ? XCircle :
    status === "warning" ? AlertTriangle : Circle;
  const tone =
    status === "good" ? "text-emerald-500" :
    status === "critical" ? "text-red-500" :
    status === "warning" ? "text-amber-500" :
    isDark ? "text-slate-400" : "text-gray-400";

  return (
    <div className={`flex gap-3 rounded-md p-3 ${isDark ? "bg-slate-900" : "bg-gray-50"}`}>
      <Icon size={18} className={`${tone} shrink-0 mt-0.5`} aria-hidden />
      <div>
        <p className={`text-sm font-semibold ${tone}`}>{title}</p>
        <p className={`text-xs mt-0.5 ${isDark ? "text-slate-400" : "text-gray-600"}`}>{detail}</p>
      </div>
    </div>
  );
}

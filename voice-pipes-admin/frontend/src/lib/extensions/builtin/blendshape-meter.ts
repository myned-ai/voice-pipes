// Built-in Playground Extension: a bar meter for blendshape weights.
//
// Purpose: validate an `AudioPostProcessor` / `AudioAnnotator` (e.g. the
// wav2arkit avatar-sync service) WITHOUT any avatar. Pure DOM + <canvas>, no
// dependencies. Also the reference implementation for the extension API (late
// mount, theme, RAF only while there is data, clean unmount).
//
// Two sources, two timelines (never mixed):
//
//   inline     `audioChunk.meta.extras.blendshapes` — keyframes in absolute
//              AudioContext seconds; frame i plays at `chunk.startTime + i / fps`.
//   annotation `audioAnnotation` with `kind === "blendshapes"` — keyframes in
//              utterance-offset milliseconds; frame i plays at
//              `startMs + i * 1000 / fps` of `utteranceId`, read against
//              `playbackClock` snapshots (`nowMs` / `utteranceId`).
//
// When the annotation timeline has frames for the utterance the clock is
// currently playing, it wins; otherwise the meter falls back to the inline
// timeline. The header line says which one is active (`src=annotation` /
// `src=inline`).
//
// Accepted blendshape payload shapes (both sources; for annotations the
// payload is `extras.blendshapes` when present, else `extras` itself):
//   { fps: number, names?: string[], frames: number[][] }  — keyframes covering
//       the chunk / span
//   Record<string, number>                                 — one weight map for
//       the whole chunk / span

import type { PlaybackChunk } from "../../audio/playback";
import type { ClockSnapshot } from "../../audio/playback-clock";
import { emitRtf } from "../../metrics/lookup";
import type { ClientPlaybackMetrics } from "../../sdk/VoicePipesSessionCore";
import type { AudioAnnotation } from "../../transport/types";
import type { PlaygroundExtension, PlaygroundExtensionHost } from "../types";

export interface Keyframe {
  /** Absolute AudioContext time (seconds). */
  t: number;
  utteranceId: number | undefined;
  weights: Map<string, number>;
}

export interface Timeline {
  frames: Keyframe[];
  fps: number | null;
}

/** One annotation keyframe in utterance-offset space. */
export interface OffsetKeyframe {
  /** Offset from the start of the utterance's audio (milliseconds). */
  tMs: number;
  utteranceId: number;
  weights: Map<string, number>;
}

export interface OffsetTimeline {
  frames: OffsetKeyframe[];
  fps: number | null;
}

export type BlendshapeSource = "inline" | "annotation";

/** What the meter renders on one paint: the chosen frame and where it came from. */
export interface BlendshapePick {
  frame: { utteranceId: number | undefined; weights: Map<string, number> } | null;
  source: BlendshapeSource;
  /** Frames in the active timeline. */
  count: number;
  fps: number | null;
  /** Playhead label for the header (`t=160ms` / `t=1.23s`). */
  at: string;
}

/** Keep at most this many seconds of history behind the playhead. */
const PRUNE_AFTER_S = 2;
/** Same bound for the offset timeline (ms). */
const PRUNE_AFTER_MS = PRUNE_AFTER_S * 1000;
/** Annotations may arrive ahead of playback; keep at most this many
 *  utterances' worth of offset timelines around. */
const MAX_OFFSET_UTTERANCES = 8;
/** After this much chunk audio with no extras, show the "no extras" hint. */
const NO_EXTRAS_HINT_AFTER_MS = 3000;
/** The annotation kind this meter renders. */
const BLENDSHAPES_KIND = "blendshapes";

export interface BlendshapeMeterOptions {
  maxBars?: number;
  highlight?: string[];
}

/** Parse a `{ fps, names?, frames }` / flat-weight-map payload into weight
 *  maps + the relative offset (seconds from the span start) of each. */
function parseWeightFrames(raw: unknown): { fps: number | null; entries: Array<{ dt: number; weights: Map<string, number> }> } {
  if (raw == null || typeof raw !== "object" || Array.isArray(raw)) return { fps: null, entries: [] };
  const obj = raw as Record<string, unknown>;

  if (Array.isArray(obj.frames)) {
    const fps = typeof obj.fps === "number" && obj.fps > 0 ? obj.fps : 30;
    const names = Array.isArray(obj.names) ? (obj.names as unknown[]).map(String) : null;
    const entries: Array<{ dt: number; weights: Map<string, number> }> = [];
    (obj.frames as unknown[]).forEach((f, i) => {
      if (!Array.isArray(f)) return;
      const weights = new Map<string, number>();
      (f as unknown[]).forEach((v, j) => {
        if (typeof v === "number") weights.set(names?.[j] ?? `#${j}`, v);
      });
      entries.push({ dt: i / fps, weights });
    });
    return { fps, entries };
  }

  // Flat weight map for the whole chunk / span.
  const weights = new Map<string, number>();
  for (const [k, v] of Object.entries(obj)) if (typeof v === "number") weights.set(k, v);
  if (weights.size === 0) return { fps: null, entries: [] };
  return { fps: null, entries: [{ dt: 0, weights }] };
}

/** Parse one chunk's extras into absolute-time keyframes. Exported for tests. */
export function keyframesFromChunk(chunk: PlaybackChunk): { frames: Keyframe[]; fps: number | null } {
  const { fps, entries } = parseWeightFrames(chunk.meta.extras?.blendshapes);
  const utteranceId = chunk.meta.utteranceId;
  return {
    fps,
    frames: entries.map(({ dt, weights }) => ({ t: chunk.startTime + dt, utteranceId, weights })),
  };
}

/** Parse one `blendshapes` annotation into utterance-offset keyframes: frame i
 *  at `startMs + i * 1000 / fps`. The payload is `extras.blendshapes` when
 *  present, else `extras` itself. Other kinds yield no frames. Exported for tests. */
export function keyframesFromAnnotation(a: AudioAnnotation): { frames: OffsetKeyframe[]; fps: number | null } {
  if (a.kind !== BLENDSHAPES_KIND) return { frames: [], fps: null };
  const nested = a.extras.blendshapes;
  const raw = nested != null && typeof nested === "object" && !Array.isArray(nested) ? nested : a.extras;
  const { fps, entries } = parseWeightFrames(raw);
  return {
    fps,
    frames: entries.map(({ dt, weights }) => ({ tMs: a.startMs + dt * 1000, utteranceId: a.utteranceId, weights })),
  };
}

/** Pick the keyframe at or before `now` (last one whose t <= now). Exported for tests. */
export function frameAt(frames: readonly Keyframe[], now: number): Keyframe | null {
  let best: Keyframe | null = null;
  for (const f of frames) {
    if (f.t <= now) best = f;
    else break;
  }
  return best;
}

/** Offset-space twin of `frameAt`: last keyframe whose `tMs <= nowMs`. Exported for tests. */
export function frameAtOffset(frames: readonly OffsetKeyframe[], nowMs: number): OffsetKeyframe | null {
  let best: OffsetKeyframe | null = null;
  for (const f of frames) {
    if (f.tMs <= nowMs) best = f;
    else break;
  }
  return best;
}

/** Source selection: when the annotation timeline has frames for the
 *  utterance the clock is playing, render from it at `clock.nowMs`; otherwise
 *  fall back to the inline timeline at the AudioContext time `audioNowS`.
 *  The two timelines are never mixed. Exported for tests. */
export function pickFrame(
  clock: ClockSnapshot | null,
  offsetTimelines: ReadonlyMap<number, OffsetTimeline>,
  inline: Timeline,
  audioNowS: number,
): BlendshapePick {
  const uttId = clock?.utteranceId ?? null;
  const ann = uttId != null ? offsetTimelines.get(uttId) : undefined;
  if (ann && ann.frames.length > 0 && clock) {
    return { frame: frameAtOffset(ann.frames, clock.nowMs), source: "annotation", count: ann.frames.length, fps: ann.fps, at: `t=${Math.round(clock.nowMs)}ms` };
  }
  return { frame: frameAt(inline.frames, audioNowS), source: "inline", count: inline.frames.length, fps: inline.fps, at: `t=${audioNowS.toFixed(2)}s` };
}

export function createBlendshapeMeter(): PlaygroundExtension {
  let host: PlaygroundExtensionHost | null = null;
  let container: HTMLElement | null = null;
  let canvas: HTMLCanvasElement | null = null;
  let ctx2d: CanvasRenderingContext2D | null = null;
  let audioContext: AudioContext | null = null;
  /** Inline source: AudioContext-time keyframes. */
  const timeline: Timeline = { frames: [], fps: null };
  /** Annotation source: one offset-space timeline per utterance. */
  const offsetTimelines = new Map<number, OffsetTimeline>();
  /** Latest playback-clock snapshot (the playhead for the annotation source). */
  let clock: ClockSnapshot | null = null;
  let clockUnsub: (() => void) | null = null;
  const unsubs: Array<() => void> = [];
  let raf: number | null = null;
  let isDark = true;
  let opts: BlendshapeMeterOptions = {};
  let audioMsWithoutExtras = 0;
  let sawExtras = false;
  let lastUtterance: number | undefined;
  /** Client playback timings of the last turn (dogfoods the `playbackMetrics` event). */
  let lastTimings: ClientPlaybackMetrics | null = null;
  /** `output.emit_rtf` of the last turn (dogfoods `metricsEmitted`):
   *  > 1.0 means the server emitted
   *  audio slower than it plays — exactly what a slow lip-sync post-processor
   *  does, and what this meter exists to validate. */
  let lastEmitRtf: number | undefined;

  const now = (): number => audioContext?.currentTime ?? 0;
  const hasAnnotationFrames = (): boolean => {
    for (const tl of offsetTimelines.values()) if (tl.frames.length > 0) return true;
    return false;
  };

  function scheduleDraw(): void {
    if (raf != null || typeof requestAnimationFrame !== "function") return;
    raf = requestAnimationFrame(() => {
      raf = null;
      draw();
      // Keep animating only while frames remain ahead of / around the playhead.
      if (timeline.frames.length > 0 || hasAnnotationFrames()) scheduleDraw();
    });
  }

  function prune(): void {
    const cutoff = now() - PRUNE_AFTER_S;
    let i = 0;
    while (i < timeline.frames.length && timeline.frames[i].t < cutoff) i++;
    // Keep the frame just before cutoff so a paused playhead still shows a face.
    if (i > 1) timeline.frames.splice(0, i - 1);
    if (timeline.frames.length === 1 && timeline.frames[0].t < cutoff - PRUNE_AFTER_S) timeline.frames.length = 0;
  }

  /** Drop offset timelines of utterances older than the one the clock plays,
   *  and history far behind the playhead inside the current one. */
  function pruneOffset(): void {
    const uttId = clock?.utteranceId ?? null;
    if (uttId != null) {
      for (const key of [...offsetTimelines.keys()]) if (key < uttId) offsetTimelines.delete(key);
      const current = offsetTimelines.get(uttId);
      if (current && clock) {
        const cutoff = clock.nowMs - PRUNE_AFTER_MS;
        let i = 0;
        while (i < current.frames.length && current.frames[i].tMs < cutoff) i++;
        if (i > 1) current.frames.splice(0, i - 1);
      }
    }
    // Bound the look-ahead: annotations for utterances that never play.
    while (offsetTimelines.size > MAX_OFFSET_UTTERANCES) {
      const oldest = Math.min(...offsetTimelines.keys());
      offsetTimelines.delete(oldest);
    }
  }

  function onChunk(chunk: PlaybackChunk): void {
    lastUtterance = chunk.meta.utteranceId ?? lastUtterance;
    const { frames, fps } = keyframesFromChunk(chunk);
    if (frames.length === 0) {
      if (!sawExtras) audioMsWithoutExtras += Math.max(0, (chunk.endTime - chunk.startTime) * 1000);
      if (timeline.frames.length === 0 && !hasAnnotationFrames()) draw();
      return;
    }
    sawExtras = true;
    if (fps != null) timeline.fps = fps;
    timeline.frames.push(...frames);
    timeline.frames.sort((a, b) => a.t - b.t);
    scheduleDraw();
  }

  function onAnnotation(a: AudioAnnotation): void {
    const { frames, fps } = keyframesFromAnnotation(a);
    if (frames.length === 0) return;
    sawExtras = true;
    let tl = offsetTimelines.get(a.utteranceId);
    if (!tl) {
      tl = { frames: [], fps: null };
      offsetTimelines.set(a.utteranceId, tl);
    }
    if (fps != null) tl.fps = fps;
    // Batches may complete out of order (bounded concurrency on the hub).
    tl.frames.push(...frames);
    tl.frames.sort((x, y) => x.tMs - y.tMs);
    pruneOffset();
    scheduleDraw();
  }

  function onClock(snap: ClockSnapshot): void {
    clock = snap;
    if (hasAnnotationFrames()) {
      pruneOffset();
      scheduleDraw();
    }
  }

  function bindClock(): void {
    clockUnsub?.();
    clockUnsub = null;
    clock = null;
    const pc = host?.session.playbackClock ?? null;
    if (pc) clockUnsub = pc.subscribe(onClock);
  }

  function clear(): void {
    timeline.frames.length = 0;
    offsetTimelines.clear();
    draw();
  }

  function draw(): void {
    if (!ctx2d || !canvas || !container) return;
    const dpr = typeof devicePixelRatio === "number" ? devicePixelRatio : 1;
    const w = container.clientWidth || 300;
    const h = container.clientHeight || 150;
    if (canvas.width !== Math.floor(w * dpr) || canvas.height !== Math.floor(h * dpr)) {
      canvas.width = Math.floor(w * dpr);
      canvas.height = Math.floor(h * dpr);
      canvas.style.width = `${w}px`;
      canvas.style.height = `${h}px`;
    }
    const g = ctx2d;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const bg = isDark ? "#020617" : "#ffffff";
    const fg = isDark ? "#cbd5e1" : "#334155";
    const dim = isDark ? "#334155" : "#e2e8f0";
    const bar = isDark ? "#818cf8" : "#4f46e5";
    const hot = isDark ? "#f59e0b" : "#d97706";
    g.fillStyle = bg;
    g.fillRect(0, 0, w, h);
    g.font = "11px ui-monospace, SFMono-Regular, Menlo, monospace";
    g.textBaseline = "top";

    prune();
    pruneOffset();
    const { frame, source, count, fps, at } = pickFrame(clock, offsetTimelines, timeline, now());
    const timings = lastTimings
      ? ` · ttfa=${lastTimings.clientTtfaMs ?? "–"} buffer=${lastTimings.clientReceiveToPlayMs ?? "–"} delivery=${lastTimings.clientPlaybackLatencyMs ?? "–"}ms`
      : "";
    const throughput = lastEmitRtf !== undefined
      ? ` · emit_rtf=${lastEmitRtf.toFixed(2)}${lastEmitRtf > 1 ? " SLOW" : ""}`
      : "";
    const header = frame
      ? `utt ${frame.utteranceId ?? lastUtterance ?? "?"} · ${count} frames · ${fps ?? "–"} fps · src=${source} ${at}${timings}${throughput}`
      : sawExtras
        ? `idle — waiting for the next ${source === "annotation" ? "annotation" : "chunk"} with blendshapes (src=${source})`
        : audioMsWithoutExtras >= NO_EXTRAS_HINT_AFTER_MS
          ? "no extras on this session — is an AudioPostProcessor or AudioAnnotator configured on the worker?"
          : "waiting for audio…";
    g.fillStyle = fg;
    g.fillText(header, 8, 6);

    if (!frame) return;
    const names = [...frame.weights.keys()].sort();
    const maxBars = typeof opts.maxBars === "number" && opts.maxBars > 0 ? opts.maxBars : names.length;
    const shown = names.slice(0, maxBars);
    if (shown.length === 0) return;
    const highlight = new Set(opts.highlight ?? []);
    const top = 24;
    const bottom = h - 4;
    const labelH = 40;
    const plotH = Math.max(10, bottom - top - labelH);
    const bw = (w - 16) / shown.length;
    // Top-N labels: draw names for the biggest few, or all when there is room.
    const ranked = [...shown].sort((a, b) => (frame.weights.get(b) ?? 0) - (frame.weights.get(a) ?? 0));
    const labelSet = new Set(bw >= 40 ? shown : ranked.slice(0, 6));
    shown.forEach((name, i) => {
      const v = Math.max(0, Math.min(1, frame.weights.get(name) ?? 0));
      const x = 8 + i * bw;
      g.fillStyle = dim;
      g.fillRect(x + 1, top, Math.max(1, bw - 2), plotH);
      g.fillStyle = highlight.has(name) ? hot : bar;
      const bh = v * plotH;
      g.fillRect(x + 1, top + plotH - bh, Math.max(1, bw - 2), bh);
      if (labelSet.has(name)) {
        g.save();
        g.translate(x + bw / 2 + 4, top + plotH + 4);
        g.rotate(Math.PI / 2);
        g.fillStyle = fg;
        g.fillText(name.length > 14 ? `${name.slice(0, 13)}…` : name, 0, 0);
        g.restore();
      }
    });
  }

  return {
    id: "voice-pipes.blendshape-meter",
    name: "Blendshapes",
    version: "1.1.0",
    slot: "stage",
    mount(el, h) {
      host = h;
      container = el;
      opts = (h.options as BlendshapeMeterOptions) ?? {};
      isDark = h.theme.isDark;
      canvas = document.createElement("canvas");
      canvas.style.display = "block";
      el.appendChild(canvas);
      // jsdom returns null here; the meter must stay inert rather than throw.
      ctx2d = typeof canvas.getContext === "function" ? canvas.getContext("2d") : null;

      // Late mount into a live session: bind to whatever is playing now.
      audioContext = h.session.playbackService?.audioContext ?? null;
      bindClock();
      unsubs.push(
        h.session.on("audioChunk", onChunk),
        h.session.on("audioAnnotation", onAnnotation),
        h.session.on("playbackChanged", (pb) => {
          audioContext = pb?.audioContext ?? null;
          // The clock is recreated together with the playback service.
          bindClock();
          if (!pb) clear();
        }),
        h.session.on("audioFlushed", clear),
        h.session.on("sessionTerminated", clear),
        h.session.on("playbackMetrics", (m) => { lastTimings = m; }),
        h.session.on("metricsEmitted", (_turnId, m) => { lastEmitRtf = emitRtf(m.snapshot); }),
        h.theme.onChange((d) => { isDark = d; draw(); }),
      );
      draw();
    },
    resize() {
      draw();
    },
    unmount() {
      if (raf != null && typeof cancelAnimationFrame === "function") cancelAnimationFrame(raf);
      raf = null;
      clockUnsub?.();
      clockUnsub = null;
      clock = null;
      for (const u of unsubs.splice(0)) {
        try { u(); } catch (e) { host?.log("warn", "unsubscribe failed", e); }
      }
      canvas?.remove();
      canvas = null;
      ctx2d = null;
      container = null;
      host = null;
      timeline.frames.length = 0;
      offsetTimelines.clear();
      lastTimings = null;
      lastEmitRtf = undefined;
    },
  };
}

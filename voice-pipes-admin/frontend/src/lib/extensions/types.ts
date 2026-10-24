// Playground Extensions — public contract for third-party renderers/visualisers.
// Framework-agnostic on purpose (vanilla DOM + callbacks). Additive-only from v1.
// See docs/clients/playground-extensions.md.

import type { AudioPlaybackService, PlaybackChunk } from "../audio/playback";
import type { PlaybackClock } from "../audio/playback-clock";
import type { AudioAnnotation, NegotiatedConfig, PipelineMessage, PipelineState, ClientToolPhase } from "../transport/types";
import type { TurnMetricsEnvelope, ClientPlaybackMetrics } from "../sdk/VoicePipesSessionCore";

export const PLAYGROUND_EXTENSION_API_VERSION = 1 as const;

export type SessionLifecycleState = "idle" | "connecting" | "active" | "error";

/** Events an extension may subscribe to (subset of the core's event map + two chunk-level events). */
export interface PlaygroundExtensionEventMap {
  audioChunk: (chunk: PlaybackChunk) => void;
  playbackChanged: (playback: AudioPlaybackService | null) => void;
  stateChanged: (state: PipelineState | null) => void;
  sessionStateChanged: (state: SessionLifecycleState) => void;
  audioFlushed: (args: { utteranceId: number; playedMs: number; reason: "barge_in" | "cancel" | "disconnect" }) => void;
  playbackFinished: (args: { utteranceId: number }) => void;
  sessionReady: (negotiated: NegotiatedConfig) => void;
  sessionTerminated: (reason: string) => void;
  pipelineMessage: (message: PipelineMessage) => void;
  /** Server `turn_metrics` snapshot + client-measured timings for one turn. */
  metricsEmitted: (turnId: string, metrics: TurnMetricsEnvelope) => void;
  /** Client playback timings (receive→play, TTFA, delivery) — fired when an
   *  utterance's first chunk is scheduled, and again when the turn ends
   *  (natural end or flush) with `clientArrivalRtf` added; may precede or
   *  follow `metricsEmitted` for the same utterance. */
  playbackMetrics: (metrics: ClientPlaybackMetrics) => void;
  /** One `audio_annotation` frame from an off-path annotator (blendshapes,
   *  visemes, …) in utterance-offset space: place `startMs + i·(1000/fps)`
   *  against `playbackClock` snapshots (`nowMs`) with the same `utteranceId`.
   *  May precede or follow the audio it describes; never fires for a flushed
   *  utterance. ~6 Hz per annotator — keep it out of React state. */
  audioAnnotation: (annotation: AudioAnnotation) => void;
}

/** Narrow, read-mostly view of the live session. Never the core itself. */
export interface PlaygroundExtensionSession {
  readonly sessionState: SessionLifecycleState;
  readonly pipelineState: PipelineState | null;
  /** May change per session — listen to `playbackChanged` instead of caching. */
  readonly playbackService: AudioPlaybackService | null;
  readonly playbackClock: PlaybackClock | null;
  readonly activeUtteranceId: number | null;
  /** Subscribe; returns an unsubscribe function. Safe to call before a session exists. */
  on<K extends keyof PlaygroundExtensionEventMap>(event: K, listener: PlaygroundExtensionEventMap[K]): () => void;
  /** Extensions may act as client-tool executors (e.g. an avatar reacting to `trigger_action`). */
  sendClientToolResult(callId: string, phase: ClientToolPhase, result?: unknown, speak?: boolean): void;
}

export interface PlaygroundExtensionTheme {
  readonly isDark: boolean;
  onChange(listener: (isDark: boolean) => void): () => void;
}

export interface PlaygroundExtensionHost {
  readonly apiVersion: typeof PLAYGROUND_EXTENSION_API_VERSION;
  readonly session: PlaygroundExtensionSession;
  readonly theme: PlaygroundExtensionTheme;
  /** Free-form options from the manifest entry. */
  readonly options: Readonly<Record<string, unknown>>;
  /** Absolute URL of the extension module — resolve relative asset paths against it. */
  readonly baseUrl: string;
  /** Structured logger routed to the console with an `[ext:<id>]` prefix (and to the Event Log in a later version). */
  readonly log: (level: "debug" | "info" | "warn" | "error", message: string, detail?: unknown) => void;
}

export interface PlaygroundExtension {
  /** Reverse-DNS-ish unique id, e.g. "myned.avatar". */
  id: string;
  /** Tab / panel label. */
  name: string;
  version?: string;
  /** Rendering slot. v1 supports only "stage" (panel above the transcript). */
  slot?: "stage";
  /** Called once per mount with a sized, empty container the extension fully owns. */
  mount(container: HTMLElement, host: PlaygroundExtensionHost): void | Promise<void>;
  /** Must release everything (WebGL contexts, subscriptions, timers). Container is discarded afterwards. */
  unmount(): void | Promise<void>;
  /** Optional hint after the container was resized (extensions may also use ResizeObserver). */
  resize?(width: number, height: number): void;
  /**
   * Optional. Resolves once the extension can render meaningfully (heavy
   * assets loaded, WebGL context up). The Playground may hold the *opening*
   * of a session until every mounted extension is ready — bounded by
   * `EXTENSION_READY_TIMEOUT_MS` — so an agent that greets on connect is not
   * speaking to a loading spinner. Called once, after `mount()` resolved.
   * MUST NOT reject and MUST NOT hang on failure: resolve when you give up.
   * Absent = ready as soon as `mount()` resolves.
   */
  ready?(): Promise<void>;
}

/** What `import(url)` must resolve to. */
export type PlaygroundExtensionModule = {
  default: PlaygroundExtension | (() => PlaygroundExtension);
};

/** Manifest file shape (`/playground-extensions.json`). */
export interface PlaygroundExtensionManifest {
  extensions: PlaygroundExtensionManifestEntry[];
}
export interface PlaygroundExtensionManifestEntry {
  /** ESM URL (absolute, or root-relative to the admin frontend origin) or `builtin:<name>`. */
  url: string;
  /** Passed to the extension as `host.options`. */
  options?: Record<string, unknown>;
  /** Default false; when true the entry is listed but not mounted until the user enables it in the stage. */
  disabled?: boolean;
}

/** Type-safe identity helper for extension authors. */
export function definePlaygroundExtension(ext: PlaygroundExtension): PlaygroundExtension { return ext; }

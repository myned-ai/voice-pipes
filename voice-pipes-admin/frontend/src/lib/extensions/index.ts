// Single import for Playground Extension authors (and, once vp publishes an
// npm package, its entry point).
export * from "./types";
export { createExtensionSession } from "./session-facade";
export {
  discoverExtensionEntries,
  loadExtensions,
  MANIFEST_URL,
  ENV_EXTENSIONS,
  type LoadedExtension,
  type FailedExtension,
  type ExtensionLoadResult,
} from "./loader";
export { BUILTIN_EXTENSIONS } from "./builtin";
export type { PlaybackChunk, AudioPlaybackService } from "../audio/playback";
export type { TurnMetricsEnvelope, ClientPlaybackMetrics } from "../sdk/VoicePipesSessionCore";
export type { PlaybackClock, ClockSnapshot } from "../audio/playback-clock";
export type { PipelineState, PipelineMessage, NegotiatedConfig, AudioMeta, AudioAnnotation } from "../transport/types";

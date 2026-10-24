import type { VoicePipesSessionCore } from "../sdk/VoicePipesSessionCore";
import type { PlaygroundExtensionEventMap, PlaygroundExtensionSession } from "./types";

const FORWARDED = [
  "audioChunk", "playbackChanged", "stateChanged", "sessionStateChanged", "audioFlushed",
  "playbackFinished", "sessionReady", "sessionTerminated", "pipelineMessage",
  "metricsEmitted", "playbackMetrics", "audioAnnotation",
] as const satisfies readonly (keyof PlaygroundExtensionEventMap)[];

/** Wraps a core in the narrow extension-facing view. One façade per core; cheap. */
export function createExtensionSession(core: VoicePipesSessionCore): PlaygroundExtensionSession {
  return {
    get sessionState() { return core.sessionState; },
    get pipelineState() { return core.pipelineState; },
    get playbackService() { return core.playbackService; },
    get playbackClock() { return core.playbackClock; },
    get activeUtteranceId() { return core.activeUtteranceId; },
    on(event, listener) {
      if (!(FORWARDED as readonly string[]).includes(event)) {
        throw new Error(`[extensions] unknown event "${String(event)}"`);
      }
      core.on(event, listener as never);
      return () => core.off(event, listener as never);
    },
    sendClientToolResult(callId, phase, result, speak) {
      core.sendClientToolResult(callId, phase, result, speak);
    },
  };
}

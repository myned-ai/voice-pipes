// usePlaygroundSession — retrofitted wrapper over the decoupled SDK hook bindings.

import { useVoicePipesSession } from "../../lib/sdk/react/useVoicePipesSession";

// Re-export all shared types so existing consumers need no import changes.
export type {
  PlaybackOptions, TranscriptEntryType, HighlightInfo, TranscriptEntry,
  WireEvent, TurnMetricsEntry, SessionState, StateTransition, PlaygroundSession,
} from "./session/types";

export function usePlaygroundSession() {
  return useVoicePipesSession();
}
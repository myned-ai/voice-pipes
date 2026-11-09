// useVoicePipesSession.ts — React hook binding layer for VoicePipesSessionCore.
// Coordinates and bridges core headless events to unified visual states & reducers.

import { useEffect, useLayoutEffect, useMemo, useReducer, useRef, useState, useCallback } from "react";
import { VoicePipesSessionCore, type ClientPlaybackMetrics, type VoicePipesSessionEventMap } from "../VoicePipesSessionCore";
import type { AudioProcessingOptions } from "../../audio/capture";
import { mergePlaybackMetrics } from "../../../pages/playground/session/metricsMerge";
import { MIN_MS_PER_WORD, SILENCE_LENIENCY_THRESHOLD_MS } from "../../audio/word-timing-table";
import {
  transcriptReducer,
  initialTranscriptState,
} from "../../../pages/playground/transcriptReducer";
import {
  type SessionConfig,
  type QualityTier,
  type AudioMeta,
  type ComponentMetricDescriptor,
  type ErrorComponent,
  type PipelineState,
} from "../../../lib/transport/types";
import {
  type SessionState,
  type StateTransition,
  type TurnMetricsEntry,
  type WireEvent,
  type HighlightInfo,
  type PlaybackOptions,
} from "../../../pages/playground/session/types";
import { type TransportType } from "../../../lib/transport";
import { createExtensionSession } from "../../extensions/session-facade";

export function useVoicePipesSession() {
  // Stable SDK Core controller instantiation
  const sdk = useMemo(() => new VoicePipesSessionCore(), []);
  // Narrow façade handed to Playground Extensions — stable for the hook's lifetime.
  const extensionSession = useMemo(() => createExtensionSession(sdk), [sdk]);

  // Hook-local visual states
  const [sessionState, setSessionState] = useState<SessionState>("idle");
  const [pipelineState, setPipelineState] = useState<PipelineState | null>(null);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [rttMs, setRttMs] = useState<number | null>(null);
  /** Fidelity tier the server last reported. Stays "full" unless adaptive
   *  quality is enabled server-side. */
  const [qualityTier, setQualityTier] = useState<QualityTier>("full");
  const [currentAudioMeta, setCurrentAudioMeta] = useState<AudioMeta | null>(null);
  const [highlightInfo, setHighlightInfo] = useState<HighlightInfo | null>(null);
  const [turnMetrics, setTurnMetrics] = useState<TurnMetricsEntry[]>([]);
  const [metricDescriptors, setMetricDescriptors] = useState<ComponentMetricDescriptor[]>([]);
  const [stateTimeline, setStateTimeline] = useState<StateTransition[]>([]);

  // Sound Levels (Visualisation)
  const [micActive, setMicActive] = useState(false);
  const [micLevel, setMicLevel] = useState(0);
  const [playbackLevel, setPlaybackLevel] = useState(0);

  // Connection-manager level playback parameters
  const [activeUtteranceId, setActiveUtteranceId] = useState<number | null>(null);

  // Transcript state managed via the existing pure reducer
  const [transcriptState, dispatchTranscript] = useReducer(
    transcriptReducer,
    initialTranscriptState
  );

  // Non-rendering logging buffer for high-frequency events
  const eventsRef = useRef<WireEvent[]>([]);
  const [events, setEvents] = useState<WireEvent[]>([]);

  // Synchronous mirror of speakingEntryIndex
  const speakingEntryRef = useRef(-1);
  const speakingEntryIndex = transcriptState.speakingEntryIndex ?? -1;

  // Keep up-to-date refs to avoid stale closures in event listeners. Synced
  // in layout effects (not during render): they run synchronously after
  // commit, before the browser can deliver the next transport message.
  const transcriptStateRef = useRef(transcriptState);
  useLayoutEffect(() => {
    transcriptStateRef.current = transcriptState;
  }, [transcriptState]);

  const highlightInfoRef = useRef(highlightInfo);
  useLayoutEffect(() => {
    highlightInfoRef.current = highlightInfo;
  }, [highlightInfo]);

  // Previous pipeline state for the timeline; owned by `onStateChanged`, which
  // is the only writer of `pipelineState`.
  const pipelineStateRef = useRef<PipelineState | null>(null);

  useEffect(() => {
    speakingEntryRef.current = speakingEntryIndex;
  }, [speakingEntryIndex]);

  // Handle Event Tracing with Performance Bypass for high frequency frames
  const handleEventEmitted = useCallback((dir: "in" | "out", type: string, summary: string, detail?: unknown) => {
    const newEvent: WireEvent = {
      direction: dir,
      type,
      summary,
      detail,
      timestamp: Date.now(),
    };
    eventsRef.current.push(newEvent);

    // Bypass react commits for high frequency frame logging packages
    if (type !== "audio" && type !== "mic" && type !== "annotation") {
      setEvents([...eventsRef.current]);
    }
  }, []);

  // Set up SDK Core subscriptions and bridge events to visual reducer dispatches
  useEffect(() => {
    // Synchronize levels from SDK Core polling
    const interval = setInterval(() => {
      setMicActive(sdk.micActive);
      setMicLevel(sdk.micLevel);
      setPlaybackLevel(sdk.playbackLevel);
      setCurrentAudioMeta(sdk.currentAudioMeta);
      setRttMs(sdk.rttMs);
      // Synchronize activeUtteranceId
      const targetId = speakingEntryRef.current >= 0 ? sdk.activeUtteranceId : null;
      setActiveUtteranceId(targetId);
    }, 50);

    const onSessionStateChanged: VoicePipesSessionEventMap["sessionStateChanged"] = (state) => {
      setSessionState(state);
      if (state === "error") {
        setErrorMessage(sdk.errorMessage);
      } else {
        setErrorMessage(null);
      }
    };

    const onStateChanged: VoicePipesSessionEventMap["stateChanged"] = (state) => {
      const prevState = pipelineStateRef.current;
      pipelineStateRef.current = state;
      setPipelineState(state);

      // Log State transitions into timeline sequence
      setStateTimeline((prev) => {
        if (prev.length > 0 && prev[prev.length - 1].state === state) return prev;
        const now = Date.now();
        const last = prev[prev.length - 1];
        return [
          ...prev,
          {
            state,
            prevState,
            timestamp: now,
            sincePrevMs: last ? now - last.timestamp : null,
          },
        ];
      });

      // Pass state_changed to the transcript reducer for placeholder updates
      if (state === "listening") {
        dispatchTranscript({ type: "CLEANUP_EMPTY_PLACEHOLDERS" });
      }
    };

    const onTextDelta: VoicePipesSessionEventMap["textDelta"] = ({ text, isFinal, role, messageId }) => {
      if (role === "user") {
        if (isFinal) {
          const safeId = typeof messageId === "string" ? messageId : "";
          let parsedId: number | undefined;
          if (safeId.startsWith("stt-")) {
            const n = parseInt(safeId.slice("stt-".length), 10);
            if (Number.isFinite(n)) parsedId = n;
          }
          dispatchTranscript({
            type: "USER_TRANSCRIBED",
            text,
            now: Date.now(),
            utteranceId: parsedId,
          });
        }
      } else if (role === "assistant") {
        const safeId = typeof messageId === "string" ? messageId : "";
        let uIdSafe = -1;
        if (safeId.startsWith("llm-")) {
          const n = parseInt(safeId.slice("llm-".length), 10);
          if (Number.isFinite(n)) uIdSafe = n;
        } else if (safeId.startsWith("rt-asst-")) {
          const rest = safeId.slice("rt-asst-".length);
          const n = parseInt(rest.split("-")[0], 10);
          if (Number.isFinite(n)) uIdSafe = n;
        }

        if (!isFinal) {
          // If no placeholder / actual agent bubble exists for this turn, prepare it first
          dispatchTranscript({
            type: "AGENT_PLACEHOLDER_CREATED",
            now: Date.now(),
            utteranceId: uIdSafe,
          });

          dispatchTranscript({
            type: "AGENT_CAPTION_APPEND",
            utteranceId: uIdSafe,
            captionText: text,
            concatRaw: safeId.startsWith("rt-"),
          });
        } else {
          // Fill completed llm responses
          const exists = transcriptStateRef.current.entries.some(
            (e) => e.role === "agent" && e.utteranceId === uIdSafe
          );
          if (exists) {
            dispatchTranscript({
              type: "AGENT_TEXT_FILL_PLACEHOLDER",
              utteranceId: uIdSafe,
              text,
            });
          } else {
            dispatchTranscript({
              type: "AGENT_FINAL_NEW",
              text,
              now: Date.now(),
              utteranceId: uIdSafe,
            });
          }
        }
      } else if (role === "system") {
        dispatchTranscript({
          type: "PIPELINE_MESSAGE",
          pipelineMessage: {
            kind: "system",
            text,
            role: "system",
            message_id: messageId,
          },
          now: Date.now(),
        });
      }
    };

    const onWordHighlighted: VoicePipesSessionEventMap["wordHighlighted"] = ({ wordIndex, utteranceId }) => {
      // Locate word reveal entry in visual transcript sequence
      const targetIndex = transcriptStateRef.current.entries.findIndex(
        (e) => e.role === "agent" && e.utteranceId === utteranceId
      );
      if (targetIndex >= 0) {
        setHighlightInfo({
          entryIndex: targetIndex,
          wordIndex,
        });
      }
    };

    const onAudioFlushed: VoicePipesSessionEventMap["audioFlushed"] = ({ utteranceId, reason, finalTrimWordIndex: passedIndex }) => {
      const wasSpeaking = (transcriptStateRef.current.speakingEntryIndex ?? -1) >= 0;

      // Demote speaking visual indicators
      dispatchTranscript({
        type: "SET_SPEAKING_ENTRY",
        index: -1,
      });

      // Clear Highlight reveal
      setHighlightInfo(null);

      // Perform local transcript freezes
      const words = transcriptStateRef.current.entries.findLast((e) => e.role === "agent" && e.utteranceId === utteranceId)?.text.split(/\s+/).filter(Boolean);
      const curHighlight = highlightInfoRef.current;
      let finalTrimWordIndex: number | null = null;

      if (passedIndex !== undefined && passedIndex !== null) {
        finalTrimWordIndex = passedIndex;
      } else if (curHighlight && curHighlight.entryIndex >= 0) {
        finalTrimWordIndex = curHighlight.wordIndex;
      } else if (words && words.length > 0) {
        // Fallback character trim
        const pb = sdk.playbackService;
        const capturedPlayedMs = pb?.getPlayedDurationMs() ?? 0;
        const totalMs = pb?.totalEnqueuedDurationMs ?? 0;

        // If playback finished almost all of the enqueued duration, keep the last word.
        if (totalMs > 0 && capturedPlayedMs >= totalMs - SILENCE_LENIENCY_THRESHOLD_MS) {
          finalTrimWordIndex = words.length - 1;
        } else {
          // Estimate the number of words heard based on the minimum ms per word
          const estimate = Math.floor(capturedPlayedMs / MIN_MS_PER_WORD);
          finalTrimWordIndex = Math.max(0, Math.min(words.length - 1, estimate - 1));
        }
      }

      if (finalTrimWordIndex !== null) {
        dispatchTranscript({
          type: "AGENT_FREEZE_AT_WORD",
          utteranceId,
          wordIndex: finalTrimWordIndex,
        });
      }

      // Dispatch Visual Interruption Dividers
      if (wasSpeaking) {
        dispatchTranscript({
          type: "INTERRUPTED_DIVIDER",
          reason,
          now: Date.now(),
        });
      }
    };

    const onPlaybackFinished: VoicePipesSessionEventMap["playbackFinished"] = () => {
      // Demote speaking visual indicators
      dispatchTranscript({
        type: "SET_SPEAKING_ENTRY",
        index: -1,
      });
    };

    const onSessionReady: VoicePipesSessionEventMap["sessionReady"] = (negotiated) => {
      setMetricDescriptors(negotiated.metric_descriptors ?? []);
    };

    const onMetricsEmitted: VoicePipesSessionEventMap["metricsEmitted"] = (_turnId, metrics) => {
      setTurnMetrics((prev) => [
        ...prev,
        {
          snapshot: metrics.snapshot,
          rttMs: metrics.rttMs,
          clientE2eMs: metrics.clientE2eMs,
          clientE2eSource: metrics.clientE2eSource,
          clientReceiveToPlayMs: metrics.clientReceiveToPlayMs,
          clientTtfaMs: metrics.clientTtfaMs,
          clientPlaybackLatencyMs: metrics.clientPlaybackLatencyMs,
          clientArrivalRtf: metrics.clientArrivalRtf,
          timestamp: Date.now(),
        },
      ]);
    };

    // Client playback timings can land AFTER the turn's `turn_metrics` (short
    // answers: the server is done while the jitter buffer still holds the
    // audio). Upsert into the existing entry; when there is none yet the core
    // merges them into the upcoming envelope instead.
    const onPlaybackMetrics = (m: ClientPlaybackMetrics) => {
      setTurnMetrics((prev) => mergePlaybackMetrics(prev, m));
    };

    const onErrorRaised: VoicePipesSessionEventMap["errorRaised"] = (_code, message, component) => {
      dispatchTranscript({
        type: "ERROR_RECEIVED",
        message,
        now: Date.now(),
        // The core forwards the server's component string verbatim; the
        // transcript renders whatever arrives, so keep the passthrough.
        component: component as ErrorComponent | undefined,
      });
    };

    // Structured PipelineMessages (tool_call / tool_result activity,
    // trigger_action client commands, cards) render inline in the
    // transcript as activity/command panels.
    const onPipelineMessage: VoicePipesSessionEventMap["pipelineMessage"] = (pm) => {
      dispatchTranscript({
        type: "PIPELINE_MESSAGE",
        pipelineMessage: pm,
        now: Date.now(),
      });
    };

    const onQualityTierChanged: VoicePipesSessionEventMap["qualityTierChanged"] = (tier) => {
      setQualityTier(tier);
    };

    sdk.on("sessionStateChanged", onSessionStateChanged);
    sdk.on("stateChanged", onStateChanged);
    sdk.on("textDelta", onTextDelta);
    sdk.on("wordHighlighted", onWordHighlighted);
    sdk.on("audioFlushed", onAudioFlushed);
    sdk.on("playbackFinished", onPlaybackFinished);
    sdk.on("sessionReady", onSessionReady);
    sdk.on("metricsEmitted", onMetricsEmitted);
    sdk.on("playbackMetrics", onPlaybackMetrics);
    sdk.on("errorRaised", onErrorRaised);
    sdk.on("eventsEmitted", handleEventEmitted);
    sdk.on("pipelineMessage", onPipelineMessage);
    sdk.on("qualityTierChanged", onQualityTierChanged);

    return () => {
      clearInterval(interval);
      sdk.off("sessionStateChanged", onSessionStateChanged);
      sdk.off("stateChanged", onStateChanged);
      sdk.off("textDelta", onTextDelta);
      sdk.off("wordHighlighted", onWordHighlighted);
      sdk.off("audioFlushed", onAudioFlushed);
      sdk.off("playbackFinished", onPlaybackFinished);
      sdk.off("sessionReady", onSessionReady);
      sdk.off("metricsEmitted", onMetricsEmitted);
      sdk.off("playbackMetrics", onPlaybackMetrics);
      sdk.off("errorRaised", onErrorRaised);
      sdk.off("eventsEmitted", handleEventEmitted);
      sdk.off("pipelineMessage", onPipelineMessage);
      sdk.off("qualityTierChanged", onQualityTierChanged);
      sdk.destroy();
    };
  }, [sdk, handleEventEmitted]);

  // Hook-level API wrapper delegating standard operations upstream to headless SDK Core
  const connect = useCallback(async (
    url: string,
    transportType: TransportType,
    config?: SessionConfig,
    playbackOptions?: PlaybackOptions
  ) => {
    eventsRef.current = [];
    setEvents([]);
    setStateTimeline([]);
    setTurnMetrics([]);
    dispatchTranscript({ type: "CLEAR_ALL" });
    setHighlightInfo(null);
    await sdk.connect(url, transportType, config, playbackOptions);
  }, [sdk]);

  const disconnect = useCallback(() => {
    sdk.disconnect();
  }, [sdk]);

  const sendText = useCallback((text: string) => {
    sdk.sendText(text);
  }, [sdk]);

  const startMic = useCallback(async (
    sampleRate?: number,
    processing?: AudioProcessingOptions,
  ) => {
    try {
      await sdk.startMic(sampleRate, processing);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("[useVoicePipesSession] startMic failed:", msg);
      setErrorMessage(`Microphone error: ${msg}`);
    }
  }, [sdk]);

  const stopMic = useCallback(() => {
    sdk.stopMic();
  }, [sdk]);

  const clearConversation = useCallback(() => {
    eventsRef.current = [];
    setEvents([]);
    setStateTimeline([]);
    setTurnMetrics([]);
    dispatchTranscript({ type: "CLEAR_ALL" });
    setHighlightInfo(null);
    sdk.clearConversation();
  }, [sdk]);

  const flushLocalAudio = useCallback(() => {
    sdk.flushLocalAudio();
  }, [sdk]);

  const cancelAgent = useCallback(() => {
    sdk.cancelAgent();
  }, [sdk]);

  const sendManualPlaybackReport = useCallback((offsetMs: number) => {
    sdk.sendManualPlaybackReport(offsetMs);
  }, [sdk]);

  const sendManualPlaybackComplete = useCallback(() => {
    sdk.sendManualPlaybackComplete();
  }, [sdk]);

  const sendClientToolResult = useCallback(
    (
      callId: string,
      phase: "started" | "progress" | "completed" | "failed",
      result?: unknown,
      speak?: boolean,
    ) => {
      sdk.sendClientToolResult(callId, phase, result, speak);
      // Persist what was chosen onto the command card's transcript
      // entry so the card keeps showing it across re-renders.
      dispatchTranscript({
        type: "CLIENT_TOOL_RESULT_RECORDED",
        callId,
        phase,
        result,
        speak: speak ?? false,
        now: Date.now(),
      });
    },
    [sdk],
  );

  return {
    sessionState,
    pipelineState,
    transcript: transcriptState.entries,
    events,
    micActive,
    micLevel,
    playbackLevel,
    currentAudioMeta,
    highlightInfo,
    speakingEntryIndex,
    errorMessage,
    turnMetrics,
    metricDescriptors,
    rttMs,
    qualityTier,
    stateTimeline,
    activeUtteranceId,
    connect,
    disconnect,
    sendText,
    startMic,
    stopMic,
    clearConversation,
    flushLocalAudio,
    cancelAgent,
    sendManualPlaybackReport,
    sendManualPlaybackComplete,
    sendClientToolResult,
    extensionSession,
  };
}

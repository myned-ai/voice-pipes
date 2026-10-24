import { useCallback, useRef, useState } from "react";
import { type TurnMetricsEntry } from "../session/types";
import { type ServerControl } from "../../../lib/transport/types";
import { type TransportAdapter } from "../../../lib/transport/adapter";

export function useMetricsTracker() {
  const [turnMetrics, setTurnMetrics] = useState<TurnMetricsEntry[]>([]);
  
  /** Timestamp (performance.now) when speech_end was fired — for client E2E. */
  const speechEndTsRef = useRef(0);
  /** Client-side E2E latency for the current turn (ms), captured on first audio. */
  const clientE2eMsRef = useRef<number | undefined>(undefined);

  const resetTurnMetrics = useCallback(() => {
    setTurnMetrics([]);
    speechEndTsRef.current = 0;
    clientE2eMsRef.current = undefined;
  }, []);

  const prepareForNewTurn = useCallback(() => {
    speechEndTsRef.current = 0;
    clientE2eMsRef.current = undefined;
  }, []);

  const markSpeechEnd = useCallback(() => {
    speechEndTsRef.current = performance.now();
  }, []);

  const markFirstAudioReceived = useCallback(() => {
    if (speechEndTsRef.current > 0 && clientE2eMsRef.current === undefined) {
      clientE2eMsRef.current = Math.round(performance.now() - speechEndTsRef.current);
    }
  }, []);

  const recordServerMetrics = useCallback((ctrl: ServerControl, rttMs: number | undefined) => {
    if (ctrl.action !== "turn_metrics") return;

    setTurnMetrics((prev) => [
      ...prev,
      {
        snapshot: ctrl.snapshot,
        rttMs,
        clientE2eMs: clientE2eMsRef.current,
        timestamp: Date.now(),
      },
    ]);
  }, []);

  const sendTimingReport = useCallback((
    transportRef: React.MutableRefObject<TransportAdapter | null>,
    utteranceId: number,
    event: string,
    detail?: unknown
  ) => {
    transportRef.current?.sendTimingReport?.(utteranceId, event, detail);
  }, []);

  return {
    turnMetrics,
    speechEndTsRef,
    clientE2eMsRef,
    resetTurnMetrics,
    prepareForNewTurn,
    markSpeechEnd,
    markFirstAudioReceived,
    recordServerMetrics,
    sendTimingReport
  };
}
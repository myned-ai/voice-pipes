import { useCallback, useRef, useState, useEffect } from "react";
import { createTransport, type TransportAdapter, type TransportType, type TransportState } from "../../../lib/transport";
import { type SessionConfig, type ServerMessage, type NegotiatedConfig } from "../../../lib/transport/types";
import { type SessionState, type PlaybackOptions, DEFAULT_PREBUFFER_MS } from "../session/types";

interface UseConnectionManagerProps {
  onMessage: (msg: ServerMessage) => void;
  onStateChange?: (state: TransportState) => void;
  onError?: (err: Error) => void;
  onPingUpdate?: (rttMs: number, serverTimestampMs: number, perfNow: number, wallNow: number) => void;
  onSessionReady?: (negotiated: NegotiatedConfig) => void;
}

export function useConnectionManager({
  onMessage,
  onStateChange,
  onError,
  onPingUpdate,
  onSessionReady,
}: UseConnectionManagerProps) {
  const transportRef = useRef<TransportAdapter | null>(null);

  const [sessionState, setSessionState] = useState<SessionState>("idle");
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [rttMs, setRttMs] = useState<number | null>(null);

  const negotiatedRef = useRef<NegotiatedConfig | null>(null);
  const lastPingSentRef = useRef<number>(0);
  const rttMsRef = useRef<number | null>(null);
  const pingIntervalRef = useRef<number>(0);

  const playbackOptionsRef = useRef<PlaybackOptions>({
    limitScheduledAudio: false,
    maxScheduledChunks: 2,
    prebufferMs: DEFAULT_PREBUFFER_MS,
  });

  const clearPingInterval = useCallback(() => {
    if (pingIntervalRef.current) {
      window.clearInterval(pingIntervalRef.current);
      pingIntervalRef.current = 0;
    }
  }, []);

  const handleMessage = useCallback((msg: ServerMessage) => {
    // Intercept control messages that belong to connection management
    if (msg.kind === "control" && "action" in msg.control) {
      const ctrl = msg.control;
      if (ctrl.action === "pong") {
        const perfNow = performance.now();
        const wallNow = Date.now();
        if (lastPingSentRef.current > 0) {
          const rtt = perfNow - lastPingSentRef.current;
          const rttRounded = Math.round(rtt);
          setRttMs(rttRounded);
          rttMsRef.current = rttRounded;
          onPingUpdate?.(rttRounded, ctrl.server_timestamp_ms, perfNow, wallNow);
          lastPingSentRef.current = 0;
        }
      } else if (ctrl.action === "session_ready") {
        if (ctrl.negotiated) {
          negotiatedRef.current = ctrl.negotiated;
          onSessionReady?.(ctrl.negotiated);
        }
        setSessionState("active");
      } else if (ctrl.action === "closing") {
        setSessionState("idle");
      } else if (ctrl.action === "error") {
        if (ctrl.severity === "fatal") {
          setErrorMessage(ctrl.message);
          setSessionState("error");
          transportRef.current?.disconnect();
        }
      }
    }
    // Pass it up to orchestrator
    onMessage(msg);
  }, [onMessage, onPingUpdate, onSessionReady]);

  const connect = useCallback(async (
    url: string,
    transportType: TransportType,
    config?: SessionConfig,
    playbackOptions?: PlaybackOptions
  ) => {
    transportRef.current?.disconnect();
    clearPingInterval();

    if (playbackOptions) {
      playbackOptionsRef.current = {
        limitScheduledAudio: playbackOptions.limitScheduledAudio,
        maxScheduledChunks: Math.max(1, playbackOptions.maxScheduledChunks),
        prebufferMs: Math.max(0, playbackOptions.prebufferMs ?? DEFAULT_PREBUFFER_MS),
      };
    }

    const transport = createTransport(transportType);
    transportRef.current = transport;

    transport.onMessage = handleMessage;
    transport.onStateChange = (state: TransportState) => {
      onStateChange?.(state);
      if (state === "disconnected") {
        setSessionState("idle");
        clearPingInterval();
      }
    };
    transport.onError = (err: Error) => {
      setErrorMessage(err.message);
      setSessionState("error");
      onError?.(err);
    };

    setErrorMessage(null);
    setSessionState("connecting");
    setRttMs(null);
    rttMsRef.current = null;

    try {
      await transport.connect(url, config);
      if (sessionState !== "active") {
        setSessionState("active"); // fallback
      }

      pingIntervalRef.current = window.setInterval(() => {
        if (transportRef.current?.state === "connected") {
          lastPingSentRef.current = performance.now();
          transportRef.current.sendControl({ action: "ping", timestamp_ms: Date.now() });
        }
      }, 5000);
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Connection failed";
      setErrorMessage(msg);
      setSessionState("error");
      onError?.(err instanceof Error ? err : new Error(msg));
    }
  }, [handleMessage, onStateChange, onError, clearPingInterval, sessionState]);

  const disconnect = useCallback(() => {
    transportRef.current?.disconnect();
    clearPingInterval();
    setSessionState("idle");
    setErrorMessage(null);
  }, [clearPingInterval]);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      transportRef.current?.disconnect();
      clearPingInterval();
    };
  }, [clearPingInterval]);

  return {
    transportRef,
    sessionState,
    errorMessage,
    rttMs,
    rttMsRef, // Exporting ref for synchronous access if needed
    negotiatedRef,
    playbackOptionsRef,
    setSessionState, // May be needed for edge cases
    setErrorMessage,
    connect,
    disconnect,
  };
}

// Transport adapter interface — abstract connection layer for playground.
// Implementations: WebSocketAdapter (now), WebTransportAdapter (future).

import type { SessionConfig, ServerMessage, ClientControl, PipelineMessage, PlaybackQuality } from "./types";

export type TransportState = "disconnected" | "connecting" | "connected";

/** Details about *why* a transport reached `disconnected`, when the underlying
 *  transport can supply them (e.g. a WebSocket `CloseEvent`). Lets the SDK tell
 *  a clean shutdown from an abnormal drop. All fields are best-effort — a
 *  transport that cannot supply them omits the argument entirely. */
export interface TransportCloseInfo {
  /** Protocol-level close code (e.g. WebSocket close code: 1000 normal,
   *  1006 abnormal/no-close-frame). */
  code?: number;
  /** Human-readable close reason supplied by the peer, if any. */
  reason?: string;
  /** True when the connection closed via a proper close handshake rather than
   *  the socket dropping. Mirrors `CloseEvent.wasClean`. */
  wasClean?: boolean;
}

export interface TransportAdapter {
  readonly state: TransportState;
  connect(url: string, config?: SessionConfig): Promise<void>;
  disconnect(): void;
  sendAudio(pcm16: Int16Array, seqNum: number): void;
  sendText(text: string, messageId: string): void;
  sendControl(control: ClientControl): void;
  sendMessage(message: PipelineMessage): void;

  /** Optional: send a per-event timing report for client-side instrumentation
   *  (`speech_start`, `playback_start`, `playback_end`, `audio_receive`, ...).
   *  Transports that do not support out-of-band telemetry may omit this. */
  sendTimingReport?(utteranceId: number, event: string, detail?: unknown): void;

  /** Optional: send a PlaybackReport when the client cancels or interrupts an
   *  utterance, telling the server how many ms of audio were heard so it can
   *  truncate its history accordingly. */
  sendPlaybackReport?(utteranceId: number, lastPlayedOffsetMs: number, lastPlayedText?: string): void;
  /** Per-turn playback-quality telemetry (underruns/silence). */
  sendPlaybackQuality?(quality: PlaybackQuality): void;

  /** Tell the server the current utterance has completely finished playing out. */
  sendPlaybackComplete?(utteranceId: number): void;

  /** Bytes queued on the socket and not yet handed to the network
   *  (`ws.bufferedAmount`). The only view either side gets of *uplink*
   *  congestion: the server can measure its own writes but is blind to ours.
   *  Transports without a comparable notion omit this. */
  readonly bufferedAmount?: number;

  /** Optional: feed a completed Ping/Pong round-trip into the transport's
   *  clock-offset estimator.  Transports that synchronize time differently
   *  (e.g. WebRTC's RTCP SR/RR) may omit this and expose their own clock. */
  recordPongSample?(
    clientSentPerf: number,
    serverTs: number,
    clientRecvPerf: number,
    clientRecvWall: number,
  ): void;

  onMessage: ((msg: ServerMessage) => void) | null;
  /** Fired on every transport state transition. On a `disconnected` transition
   *  the transport may pass a `TransportCloseInfo` describing the close; other
   *  transitions omit it. */
  onStateChange: ((state: TransportState, info?: TransportCloseInfo) => void) | null;
  onError: ((err: Error) => void) | null;
}

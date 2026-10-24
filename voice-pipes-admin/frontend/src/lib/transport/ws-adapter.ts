// WebSocket transport adapter â€” implements TransportAdapter over browser WebSocket.

import type { TransportAdapter, TransportState, TransportCloseInfo } from "./adapter";
import type { SessionConfig, ServerMessage, ClientControl, NegotiatedConfig, AudioMeta, AudioAnnotation, PipelineMessage, WordTiming, ServerControl, PlaybackQuality } from "./types";
import {
  encodeBinaryFrame,
  makeAudioHeader,
  encodePcmI16,
  decodeBinaryFrame,
  decodePcmI16,
  MSG_TYPE_AUDIO_PCM_I16,
  FLAG_END_OF_UTTERANCE,
  FLAG_END_OF_RESPONSE,
} from "./types";
import { ClockSync } from "./clock-sync";
/** Thrown when an inbound frame cannot be decoded (malformed JSON, truncated
 *  binary header, etc.).  Indicates a server-side or network problem; the
 *  client message handlers were never invoked. */
export class TransportParseError extends Error {
  readonly name = "TransportParseError";
  readonly cause?: unknown;
  constructor(message: string, cause?: unknown) {
    super(message);
    this.cause = cause;
  }
}

/** Thrown when a downstream `onMessage` handler throws while processing an
 *  otherwise-valid frame.  Indicates a client-side bug; the frame parsed
 *  successfully but business logic failed. */
export class TransportHandlerError extends Error {
  readonly name = "TransportHandlerError";
  readonly cause?: unknown;
  constructor(message: string, cause?: unknown) {
    super(message);
    this.cause = cause;
  }
}
/** Opens the URL it is given and adds no credential to it. The admin
 *  console's Playground connects through the backend's proxy
 *  (`/api/v1/playground/ws`), which the admin cookie authenticates and which
 *  presents the worker credential upstream; the browser never holds it. */
export class WebSocketAdapter implements TransportAdapter {
  private ws: WebSocket | null = null;

  /** Bytes queued on the socket and not yet flushed to the network. Rising
   *  means we are producing mic audio faster than the uplink drains it — the
   *  server cannot see this, so we report it (S5). */
  get bufferedAmount(): number {
    return this.ws?.bufferedAmount ?? 0;
  }
  private _state: TransportState = "disconnected";
  /** Metadata from a preceding audio_meta text frame, consumed by the next binary frame. */
  // FIFO queue of audio_meta envelopes awaiting their paired binary frame.
  // The server's output pacer can emit multiple audio_meta envelopes
  // back-to-back (one per fragment) before the corresponding paced binary
  // chunks arrive â€” a single-slot stash would clobber all but the last.
  // Each binary frame consumes one entry from the front of the queue.
  private pendingAudioMetas: AudioMeta[] = [];  /** Soft cap on `pendingAudioMetas` size.  Steady-state the queue holds at
   *  most a handful of entries (one per fragment paced ahead by the server).
   *  Crossing this threshold means binary frames are not arriving — almost
   *  always a server / network problem.  We log once per overflow burst and
   *  drop the oldest entry to keep memory bounded.  64 is generously above
   *  any plausible legitimate batch (~10 fragments per turn × small jitter). */
  private static readonly PENDING_AUDIO_META_SOFT_LIMIT = 64;
  private overflowWarned = false;
  /** Highest `fragment_idx` seen on a popped meta within the current
   *  utterance.  Used to log non-monotonic ordering — a strong signal of
   *  queue desync (e.g. an audio_meta was dropped or duplicated). */
  private lastPoppedFragmentIdx: number | null = null;
  private lastPoppedUtteranceId: number | null = null;
  /** Last successfully-popped audio_meta, kept as a sticky fallback for
   *  continuation chunks.  The server only sends audio_meta for the first
   *  chunk of each caption fragment (and the EOU sentinel), so all the
   *  subsequent fixed-size output chunks of the same fragment legitimately
   *  arrive with no queued meta.  We reuse the last one — minus caption,
   *  which belongs only to the fragment-start chunk — so every binary chunk
   *  carries the correct utteranceId and fragment_idx.  `caption` and
   *  `extras` are per-chunk and are stripped from the sticky copy. */
  private stickyMeta: Omit<AudioMeta, "caption" | "extras"> | null = null;  /** Negotiated output sample rate from session_ready (used for duration calc). */
  private outputSampleRate = 48000;
  /** Negotiated config from the last session_ready message. */
  negotiatedConfig: NegotiatedConfig | null = null;
  /** Clock synchronisation helper. */
  readonly clockSync = new ClockSync();

  onMessage: ((msg: ServerMessage) => void) | null = null;
  onStateChange: ((state: TransportState, info?: TransportCloseInfo) => void) | null = null;
  onError: ((err: Error) => void) | null = null;

  get state(): TransportState {
    return this._state;
  }

  private setState(s: TransportState, info?: TransportCloseInfo) {
    this._state = s;
    this.onStateChange?.(s, info);
  }

  connect(url: string, config?: SessionConfig): Promise<void> {
    return new Promise((resolve, reject) => {
      if (this.ws) {
        this.disconnect();
      }
      this.setState("connecting");

      const ws = new WebSocket(url);
      ws.binaryType = "arraybuffer";

      ws.onopen = () => {
        this.ws = ws;
        this.setState("connected");

        // Send session configuration if provided
        if (config) {
          this.sendControl({
            action: "configure",
            sample_rate: config.sample_rate,
            output_sample_rate: config.output_sample_rate,
            channels: config.channels,
            language: config.language,
            voice: config.voice,
            app_data: config.app_data,
            mode: config.mode,
            // Default on: this SDK's capture resamples, so it can honour any
            // rate the server picks. Callers can opt out explicitly.
            accepts_negotiated_input_rate:
              config.accepts_negotiated_input_rate ?? true,
          });
        }
        resolve();
      };

      ws.onerror = () => {
        const err = new Error("WebSocket connection failed");
        this.onError?.(err);
        if (this._state === "connecting") {
          reject(err);
        }
      };

      ws.onclose = (event: CloseEvent) => {
        this.ws = null;
        // Forward the close code/reason so the SDK can tell an abnormal drop
        // (no close frame, e.g. server crash / wrong-server port collision —
        // code 1006) from a clean shutdown (code 1000 / wasClean).
        this.setState("disconnected", {
          code: event.code,
          reason: event.reason,
          wasClean: event.wasClean,
        });
      };

      ws.onmessage = (event: MessageEvent) => {
        try {
          if (event.data instanceof ArrayBuffer) {
            this.handleBinary(event.data);
          } else if (typeof event.data === "string") {
            this.handleText(event.data);
          }
        } catch (err) {
          // Differentiate transport-layer parse failures (server frame is
          // malformed — a server/network problem) from handler-layer failures
          // (an onMessage callback threw — a client bug).  Both cases used to
          // flow through a single `onError(err)` call, making a malformed
          // server frame and a frontend bug indistinguishable in logs.
          if (err instanceof TransportParseError) {
            console.error("[ws-adapter] frame parse failed:", err.message, err.cause);
          } else if (err instanceof TransportHandlerError) {
            console.error("[ws-adapter] message handler threw:", err.message, err.cause);
          } else {
            console.error("[ws-adapter] unexpected onmessage error:", err);
          }
          this.onError?.(err instanceof Error ? err : new Error(String(err)));
        }
      };
    });
  }

  disconnect(): void {
    if (this.ws) {
      this.ws.close(1000, "client disconnect");
      this.ws = null;
    }
    // Clear any audio_meta envelopes still waiting for their binary pair.
    // Without this, a reused adapter instance would start the next session
    // with stale metas pre-queued, causing the first binary chunk of the
    // new session to be paired with an envelope from the previous session
    // (wrong utteranceId, wrong caption).
    this.pendingAudioMetas = [];
    this.overflowWarned = false;
    this.lastPoppedFragmentIdx = null;
    this.lastPoppedUtteranceId = null;
    this.stickyMeta = null;
    this.setState("disconnected");
  }

  sendAudio(pcm16: Int16Array, seqNum: number): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    const header = makeAudioHeader(seqNum);
    const payload = encodePcmI16(pcm16);
    const frame = encodeBinaryFrame(header, payload);
    this.ws.send(frame);
  }

  sendText(text: string, messageId: string): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    const envelope = { 
      type: "message", 
      payload: { 
        kind: "text", 
        text, 
        message_id: messageId,
        role: "user"
      } 
    };
    this.ws.send(JSON.stringify(envelope));
  }

  sendControl(control: ClientControl): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    const envelope = { type: "control", payload: control };
    this.ws.send(JSON.stringify(envelope));
  }

  sendMessage(message: PipelineMessage): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    const envelope = { type: "message", payload: message };
    this.ws.send(JSON.stringify(envelope));
  }

  /** Convenience: send a client-side timing event. */
  sendTimingReport(utteranceId: number, event: string, detail?: unknown): void {
    this.sendControl({
      action: "timing_report",
      utterance_id: utteranceId,
      event,
      client_timestamp_ms: Date.now(),
      detail,
    });
  }

  /** Convenience: report what audio was actually played before an interruption. */
  sendPlaybackReport(utteranceId: number, lastPlayedOffsetMs: number, lastPlayedText?: string): void {
    this.sendControl({
      action: "playback_report",
      utterance_id: utteranceId,
      last_played_offset_ms: lastPlayedOffsetMs,
      last_played_text: lastPlayedText,
    });
  }

  /** Per-turn playback-quality telemetry: underrun gap list in ideal-audio
   *  coordinates plus aggregates, the arrival span / interrupted flag /
   *  annotation lateness (all serde-defaulted server-side). */
  sendPlaybackQuality(quality: PlaybackQuality): void {
    this.sendControl({
      action: "playback_quality",
      utterance_id: quality.utteranceId,
      underrun_count: quality.underrunCount,
      total_silence_ms: quality.totalSilenceMs,
      audio_ms: quality.audioMs,
      heard_ms: quality.heardMs,
      wall_ms: quality.wallMs,
      arrival_wall_ms: quality.arrivalWallMs,
      interrupted: quality.interrupted,
      annotation_late_count: quality.annotationLateCount,
      annotation_late_ms_max: quality.annotationLateMsMax,
      gaps: quality.gaps.map((g) => ({ at_audio_ms: g.atAudioMs, silence_ms: g.silenceMs })),
    });
  }

  /** Tell the server the current utterance has completely finished playing out. */
  sendPlaybackComplete(utteranceId: number): void {
    this.sendControl({
      action: "playback_complete",
      utterance_id: utteranceId,
    });
  }

  /** Feed a completed Ping/Pong round-trip into the clock-offset estimator.
   *  Implements the optional `TransportAdapter.recordPongSample` so callers
   *  do not need to reach through to the underlying `ClockSync` instance. */
  recordPongSample(
    clientSentPerf: number,
    serverTs: number,
    clientRecvPerf: number,
    clientRecvWall: number,
  ): void {
    this.clockSync.addSample(clientSentPerf, serverTs, clientRecvPerf, clientRecvWall);
  }

  private handleBinary(data: ArrayBuffer): void {
    // ── Parse phase: decode the binary frame.  Failures here are server /
    //    network problems (malformed header, truncated payload).
    let header: ReturnType<typeof decodeBinaryFrame>["header"];
    let payload: Uint8Array;
    try {
      ({ header, payload } = decodeBinaryFrame(data));
    } catch (err) {
      throw new TransportParseError(
        `failed to decode binary frame (${data.byteLength} bytes): ${(err as Error)?.message ?? err}`,
        err,
      );
    }

    // ── Dispatch phase: invoke handlers.  Any throw here is a client bug.
    try {
      if (header.msgType === MSG_TYPE_AUDIO_PCM_I16) {
        const samples = decodePcmI16(payload);
        const endOfUtterance = (header.flags & FLAG_END_OF_UTTERANCE) !== 0;
        // Merge any sideband metadata that arrived in a preceding text frame.
        // Pop the oldest pending audio_meta envelope so 1:1 ordering with
        // first-chunk-of-fragment binary frames is preserved even when the
        // server pacer batches multiple metas ahead of binary chunks.
        const pendingMeta = this.pendingAudioMetas.shift();

        // Defensive diagnostics — desync detection.  These never alter the
        // happy-path behaviour; they only surface protocol-level anomalies
        // that would otherwise be silent (a silent desync of this kind went
        // unnoticed for weeks).
        if (!pendingMeta) {
          // Empty queue is expected for continuation chunks produced by
          // output chunking: the server sends audio_meta only for the first
          // chunk of each caption fragment, not for every 50 ms slice.
          // Reuse the sticky meta from the last popped entry so utteranceId
          // and fragment_idx propagate to all continuation chunks.
          // Only warn if there is no sticky meta at all (true desync: binary
          // arrived before any audio_meta was ever received this utterance).
          if (this.stickyMeta == null) {
            console.warn(
              `[ws-adapter] binary frame arrived with empty pendingAudioMetas queue and no sticky meta (seqNum=${header.seqNum}, ${samples.length} samples) — utteranceId metadata will be missing for this chunk`,
            );
          }
        } else {
          // Track fragment_idx ordering within the current utterance.  A
          // non-monotonic value (e.g. 3 → 1) means the queue is desynced
          // and downstream caption/word-timing pairing will be wrong.
          const uttId = pendingMeta.utteranceId ?? null;
          const fragIdx = typeof pendingMeta.fragment_idx === "number" ? pendingMeta.fragment_idx : null;
          if (uttId !== this.lastPoppedUtteranceId) {
            // New utterance — reset the monotonic tracker.
            this.lastPoppedUtteranceId = uttId;
            this.lastPoppedFragmentIdx = fragIdx;
          } else if (
            fragIdx != null &&
            this.lastPoppedFragmentIdx != null &&
            fragIdx < this.lastPoppedFragmentIdx
          ) {
            console.warn(
              `[ws-adapter] non-monotonic audio_meta fragment_idx: utteranceId=${uttId}, popped ${fragIdx} after ${this.lastPoppedFragmentIdx} — meta/binary pairing is desynced`,
            );
            this.lastPoppedFragmentIdx = fragIdx;
          } else if (fragIdx != null) {
            this.lastPoppedFragmentIdx = fragIdx;
          }
          // Caption AND extras belong to the chunk that carried them; only the
          // identity fields (utterance_id, fragment_idx, …) are sticky.
          const stickyBase: AudioMeta = { ...pendingMeta };
          delete stickyBase.caption;
          delete stickyBase.extras;
          this.stickyMeta = stickyBase;
        }

        // Build the effective meta: explicit pending meta takes priority,
        // then sticky meta (continuation chunk of the same fragment), then
        // an empty base (timestamp + duration only, true desync case).
        const effectiveMeta: AudioMeta = pendingMeta ?? this.stickyMeta ?? {};
        const meta: AudioMeta = {
          timestampMs: Date.now(),
          durationMs: (samples.length / this.outputSampleRate) * 1000,
          ...effectiveMeta,
          // Caption from stickyMeta must NOT bleed onto continuation chunks;
          // only the chunk that actually popped a new meta with a caption
          // should carry one.
          ...(pendingMeta == null ? { caption: undefined, extras: undefined } : {}),
        };
        this.onMessage?.({
          kind: "audio",
          samples,
          seqNum: header.seqNum,
          endOfUtterance,
          endOfResponse: (header.flags & FLAG_END_OF_RESPONSE) !== 0,
          meta,
        });
        // On the EOU sentinel: if the server embedded full_text in the audio_meta,
        // emit a synthetic text message in the SAME call stack so it is guaranteed
        // to arrive after all caption events and before state->listening.
        if (endOfUtterance && meta.fullText) {
          this.onMessage?.({
            kind: "text",
            text: meta.fullText,
            messageId: `llm-${meta.utteranceId ?? 0}`,
          });
        }
      }
    } catch (err) {
      throw new TransportHandlerError(
        `handler threw while dispatching binary frame (msgType=${header.msgType}): ${(err as Error)?.message ?? err}`,
        err,
      );
    }
  }

  private handleText(data: string): void {
    // ── Parse phase: JSON-decode the envelope.  Failures here are server /
    //    network problems (malformed JSON).
    let envelope: { type?: string; payload?: Record<string, unknown> };
    try {
      envelope = JSON.parse(data);
    } catch (err) {
      throw new TransportParseError(
        `failed to parse JSON frame (${data.length} bytes): ${(err as Error)?.message ?? err}`,
        err,
      );
    }
    const type = envelope.type as string;
    const payload = envelope.payload ?? {};

    // ── Dispatch phase: invoke handlers.  Any throw here is a client bug.
    try {
    if (type === "control") {
      // Capture negotiated config from session_ready
      if (payload.action === "session_ready" && payload.negotiated) {
        this.negotiatedConfig = payload.negotiated as NegotiatedConfig;
        this.outputSampleRate = this.negotiatedConfig.output_sample_rate;
      }
      this.onMessage?.({ kind: "control", control: payload as unknown as ServerControl });
    } else if (type === "text") {
      this.onMessage?.({
        kind: "text",
        text: (payload.text as string) ?? "",
        messageId: (payload.message_id as string) ?? "",
      });
    } else if (type === "audio_meta") {
      // Sideband metadata envelope stash for the next binary frame.
      const raw = payload as Record<string, unknown>;
      const meta: AudioMeta = {};
      
      // Strict mapping: snake_case from wire to camelCase for client
      if (raw.text != null) {
        meta.caption = {
          text: String(raw.text),
          estimated_duration_ms: raw.estimated_duration_ms as number | undefined,
          word_timings: raw.word_timings as WordTiming[] | undefined,
        };
      }
      if (raw.utterance_id != null) {
        meta.utteranceId = Number(raw.utterance_id);
      }
      if (raw.full_text != null) {
        meta.fullText = String(raw.full_text);
      }
      if (raw.fragment_idx != null) {
        meta.fragment_idx = Number(raw.fragment_idx);
      }
      if (raw.is_final != null) {
        meta.is_final = Boolean(raw.is_final);
      }
      if (raw.total_fragment_duration_ms != null) {
        meta.total_fragment_duration_ms = Number(raw.total_fragment_duration_ms);
      }
      if (raw.extras != null && typeof raw.extras === "object" && !Array.isArray(raw.extras)) {
        meta.extras = raw.extras as Record<string, unknown>;
      }
      if (raw.server_timestamp_ms != null) {
        meta.serverTimestampMs = Number(raw.server_timestamp_ms);
      }
      this.pendingAudioMetas.push(meta);

      // Soft cap: if the queue has grown beyond the threshold, binary frames
      // are not arriving fast enough (server pacer bug, network stall, or
      // a missed binary frame).  Drop the oldest entry to bound memory and
      // log once per overflow burst.  Dropping the OLDEST keeps the most
      // recent (and most likely to still be paired) metas in flight.
      if (
        this.pendingAudioMetas.length > WebSocketAdapter.PENDING_AUDIO_META_SOFT_LIMIT
      ) {
        if (!this.overflowWarned) {
          console.warn(
            `[ws-adapter] pendingAudioMetas exceeded soft limit (${WebSocketAdapter.PENDING_AUDIO_META_SOFT_LIMIT}) — dropping oldest entry; binary chunks are starving`,
          );
          this.overflowWarned = true;
        }
        this.pendingAudioMetas.shift();
      } else if (
        this.overflowWarned &&
        this.pendingAudioMetas.length <
          WebSocketAdapter.PENDING_AUDIO_META_SOFT_LIMIT / 2
      ) {
        // Queue drained back to a healthy size — re-arm the warning so a
        // later overflow burst gets logged again.
        this.overflowWarned = false;
      }
    } else if (type === "audio_annotation") {
      // Off-path annotator output. Ordered on the same socket as audio but
      // NOT paired with a binary frame — it is
      // its own message, placed by utterance offset on the consumer side.
      const raw = payload as Record<string, unknown>;
      if (
        typeof raw.utterance_id !== "number" ||
        typeof raw.start_ms !== "number" ||
        typeof raw.duration_ms !== "number" ||
        typeof raw.kind !== "string"
      ) {
        console.warn("[ws-adapter] dropping malformed audio_annotation frame", raw);
      } else {
        const annotation: AudioAnnotation = {
          utteranceId: raw.utterance_id,
          startMs: raw.start_ms,
          durationMs: raw.duration_ms,
          kind: raw.kind,
          extras:
            raw.extras != null && typeof raw.extras === "object" && !Array.isArray(raw.extras)
              ? (raw.extras as Record<string, unknown>)
              : {},
        };
        if (raw.server_timestamp_ms != null) {
          annotation.serverTimestampMs = Number(raw.server_timestamp_ms);
        }
        this.onMessage?.({ kind: "annotation", annotation });
      }
    } else if (type === "data") {
      this.onMessage?.({ kind: "data", payload });
    } else if (type === "message") {
      this.onMessage?.({ kind: "message", message: payload as unknown as PipelineMessage });
    }
    } catch (err) {
      throw new TransportHandlerError(
        `handler threw while dispatching ${type ?? "unknown"} frame: ${(err as Error)?.message ?? err}`,
        err,
      );
    }
  }
}

// WebTransport adapter stub — feature-flagged placeholder.
//
// WebTransport (HTTP/3 + QUIC) gives us bidirectional streams without
// HTTP-Upgrade overhead, prioritized streams (control vs. audio), and
// optional unreliable datagrams (ideal for low-latency PCM that tolerates
// occasional loss).  This file establishes the seam so the rest of the
// app can target `TransportAdapter` instead of `WebSocketAdapter`; the
// actual stream wiring lands in a follow-up.

import type { TransportAdapter, TransportState } from "./adapter";
import type { ServerMessage } from "./types";

export class WebTransportAdapter implements TransportAdapter {
  private _state: TransportState = "disconnected";
  private wt: WebTransport | null = null;

  onMessage: ((msg: ServerMessage) => void) | null = null;
  onStateChange: ((state: TransportState) => void) | null = null;
  onError: ((err: Error) => void) | null = null;

  get state(): TransportState {
    return this._state;
  }

  private setState(next: TransportState): void {
    if (this._state === next) return;
    this._state = next;
    this.onStateChange?.(next);
  }

  // The optional `config` parameter of `TransportAdapter.connect` is
  // omitted until the control stream that would carry it is wired up.
  async connect(url: string): Promise<void> {
    if (typeof WebTransport === "undefined") {
      const err = new Error("WebTransport is not supported in this browser");
      this.onError?.(err);
      throw err;
    }
    this.setState("connecting");
    try {
      this.wt = new WebTransport(url);
      await this.wt.ready;
      this.setState("connected");
      // TODO: open bidirectional stream for control, datagrams for audio,
      // wire up frame parsing identical to ws-adapter's binary protocol.
      this.wt.closed.then(() => this.setState("disconnected")).catch((e) => {
        this.onError?.(e instanceof Error ? e : new Error(String(e)));
        this.setState("disconnected");
      });
    } catch (e) {
      this.setState("disconnected");
      const err = e instanceof Error ? e : new Error(String(e));
      this.onError?.(err);
      throw err;
    }
  }

  disconnect(): void {
    try {
      this.wt?.close();
    } catch {
      // ignore
    }
    this.wt = null;
    this.setState("disconnected");
  }

  // ── Stub send paths ─────────────────────────────────────────────
  // Each unimplemented method intentionally throws so callers fail loudly
  // during development if the WebTransport flag is enabled before the
  // real implementation lands.

  // Each stub is typed by the `TransportAdapter` contract so callers still
  // see the real parameter list (`pcm16, seqNum`, `text, messageId`, ...),
  // while the stub body itself declares no parameters it does not use.

  sendAudio: TransportAdapter["sendAudio"] = () => {
    throw new Error("WebTransportAdapter.sendAudio: not implemented");
  };

  sendText: TransportAdapter["sendText"] = () => {
    throw new Error("WebTransportAdapter.sendText: not implemented");
  };

  sendControl: TransportAdapter["sendControl"] = () => {
    throw new Error("WebTransportAdapter.sendControl: not implemented");
  };

  sendMessage: TransportAdapter["sendMessage"] = () => {
    throw new Error("WebTransportAdapter.sendMessage: not implemented");
  };
}

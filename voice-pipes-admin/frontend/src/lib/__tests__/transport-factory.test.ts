import { describe, it, expect } from "vitest";
import {
  createTransport,
  WebSocketAdapter,
  WebTransportAdapter,
  WebRtcAdapter,
  EXPERIMENTAL_TRANSPORTS,
} from "../transport";

describe("createTransport factory", () => {
  it("returns WebSocketAdapter by default", () => {
    expect(createTransport()).toBeInstanceOf(WebSocketAdapter);
  });

  it("throws when an experimental transport is requested without opt-in", () => {
    expect(() => createTransport("webtransport")).toThrow(/experimental/i);
    expect(() => createTransport("webrtc")).toThrow(/experimental/i);
  });

  it("returns the experimental adapter when opted in", () => {
    expect(createTransport("webtransport", { allowExperimental: true })).toBeInstanceOf(
      WebTransportAdapter,
    );
    expect(createTransport("webrtc", { allowExperimental: true })).toBeInstanceOf(
      WebRtcAdapter,
    );
  });

  it("flags both new transports as experimental", () => {
    expect(EXPERIMENTAL_TRANSPORTS.has("webtransport")).toBe(true);
    expect(EXPERIMENTAL_TRANSPORTS.has("webrtc")).toBe(true);
    expect(EXPERIMENTAL_TRANSPORTS.has("websocket")).toBe(false);
  });
});

describe("experimental adapters expose the TransportAdapter contract", () => {
  it("WebTransportAdapter starts disconnected and exposes all required methods", () => {
    const a = new WebTransportAdapter();
    expect(a.state).toBe("disconnected");
    expect(typeof a.connect).toBe("function");
    expect(typeof a.disconnect).toBe("function");
    expect(typeof a.sendAudio).toBe("function");
    expect(typeof a.sendText).toBe("function");
    expect(typeof a.sendControl).toBe("function");
    expect(typeof a.sendMessage).toBe("function");
    // Stub send paths throw loudly until implemented.
    expect(() => a.sendAudio(new Int16Array(), 0)).toThrow(/not implemented/i);
  });

  it("WebRtcAdapter starts disconnected and rejects sendAudio with a clarifying error", () => {
    const a = new WebRtcAdapter();
    expect(a.state).toBe("disconnected");
    expect(() => a.sendAudio(new Int16Array(), 0)).toThrow(/RTCRtpTransceiver/);
  });
});

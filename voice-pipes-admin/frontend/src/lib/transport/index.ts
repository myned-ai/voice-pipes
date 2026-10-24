// Transport barrel export & factory.

export type { TransportAdapter, TransportState } from "./adapter";
export { WebSocketAdapter } from "./ws-adapter";
export { WebTransportAdapter } from "./webtransport-adapter";
export { WebRtcAdapter } from "./webrtc-adapter";
export * from "./types";

import type { TransportAdapter } from "./adapter";
import { WebSocketAdapter } from "./ws-adapter";
import { WebTransportAdapter } from "./webtransport-adapter";
import { WebRtcAdapter } from "./webrtc-adapter";

export type TransportType = "websocket" | "webtransport" | "webrtc";

/** Set of transport types whose adapters are stub-only. Gate behind a
 *  build/runtime flag if you intend to expose them to end users. */
export const EXPERIMENTAL_TRANSPORTS: ReadonlySet<TransportType> = new Set([
  "webtransport",
  "webrtc",
]);

export function createTransport(
  type: TransportType = "websocket",
  opts: { allowExperimental?: boolean } = {},
): TransportAdapter {
  if (EXPERIMENTAL_TRANSPORTS.has(type) && !opts.allowExperimental) {
    throw new Error(
      `Transport "${type}" is experimental; pass { allowExperimental: true } to opt in.`,
    );
  }
  switch (type) {
    case "websocket":
      return new WebSocketAdapter();
    case "webtransport":
      return new WebTransportAdapter();
    case "webrtc":
      return new WebRtcAdapter();
    default:
      throw new Error(`Unknown transport type: ${type satisfies never}`);
  }
}

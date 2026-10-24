import type { TransportCloseInfo } from "../../transport/adapter";

/** Build a human-readable message for a transport that dropped without a clean
 *  shutdown (no `closing` frame, not client-initiated). Uses the WebSocket
 *  close code/reason when available to hint at the likely cause. */
export function describeUnexpectedClose(info?: TransportCloseInfo): string {
  const base =
    "Connection closed unexpectedly — the server dropped the connection without a shutdown message. " +
    "It may have crashed, restarted, or you may be connected to the wrong server.";
  if (!info) return base;
  const parts: string[] = [];
  if (typeof info.code === "number") parts.push(`code ${info.code}`);
  if (info.reason) parts.push(`reason: ${info.reason}`);
  return parts.length > 0 ? `${base} (${parts.join(", ")})` : base;
}

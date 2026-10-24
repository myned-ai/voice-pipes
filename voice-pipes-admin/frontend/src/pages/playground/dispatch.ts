// Orchestrator dispatch for a Playground session whose source is a pipeline: ask the BFF
// (which proxies the orchestrator) for a worker serving the selected pipeline
// and return the WebSocket URL the browser should open.
//
// Kept separate from the page so the contract is testable: the selected
// pipeline MUST reach the orchestrator as `pipeline_name`, and the session
// MUST connect to the BFF's Playground proxy for the worker that comes back
// (never the default URL, and never the worker directly: the proxy presents
// the worker credential, which the browser does not hold).

import axios from "axios";

export interface DispatchTarget {
  wsUrl: string;
  workerId: string | null;
  pipelineName: string | null;
}

/** `path` (from the BFF, e.g. `/api/v1/playground/ws?worker_id=w1`) as a
 *  WebSocket URL on the BFF at `apiBase`: `http` becomes `ws`, `https` `wss`.
 *  A relative `apiBase` resolves against the page's own origin. */
export function bffWebSocketUrl(apiBase: string, path: string): string {
  const base = new URL(apiBase || "/", window.location.href);
  const url = new URL(base.pathname.replace(/\/$/, "") + path, base);
  url.protocol = base.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}

export async function requestDispatch(apiBase: string, selectedPipeline: string): Promise<DispatchTarget> {
  const params: Record<string, string> = {};
  if (selectedPipeline) params.pipeline_name = selectedPipeline;
  const res = await axios.get(`${apiBase}/api/v1/dispatch`, { params });
  const data = res.data?.data;
  const wsPath: unknown = data?.ws_path;
  if (typeof wsPath !== "string" || !wsPath.startsWith("/")) {
    throw new Error("Dispatch returned no worker — are any workers registered?");
  }
  return {
    wsUrl: bffWebSocketUrl(apiBase, wsPath),
    workerId: typeof data?.worker_id === "string" ? data.worker_id : null,
    pipelineName: typeof data?.pipeline_name === "string" ? data.pipeline_name : null,
  };
}

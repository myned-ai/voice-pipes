// Where a Playground session goes: a worker the orchestrator picks for a
// pipeline, or a URL typed by hand. The two are one choice with one Start
// button, so the rules for which is offered, and what the bar says once
// connected, live here where they can be tested without rendering anything.

export type ConnectSource = "pipeline" | "url";

/** What the orchestrator said when asked for its pipelines. */
export type PipelineListStatus = "loading" | "ok" | "unreachable";

export const CONNECT_SOURCE_KEY = "vp.playground.connectSource";

export function readPreferredSource(): ConnectSource {
  try {
    return localStorage.getItem(CONNECT_SOURCE_KEY) === "url" ? "url" : "pipeline";
  } catch {
    return "pipeline";
  }
}

export function storePreferredSource(source: ConnectSource): void {
  try {
    localStorage.setItem(CONNECT_SOURCE_KEY, source);
  } catch {
    // A remembered tab is a convenience; the bar works without it.
  }
}

/** Why the pipeline source cannot be used, or null when it can. */
export function pipelineUnavailableReason(status: PipelineListStatus, pipelineCount: number): string | null {
  if (status === "unreachable") return "Orchestrator not reachable";
  if (status === "ok" && pipelineCount === 0) return "No pipelines registered";
  return null;
}

/**
 * The source actually in force. A preference for "pipeline" holds while the
 * list is still loading, so the bar does not flip to the URL box and back on
 * every page load, and gives way only once the orchestrator is known to have
 * nothing to offer.
 */
export function effectiveSource(
  preferred: ConnectSource,
  status: PipelineListStatus,
  pipelineCount: number,
): ConnectSource {
  if (preferred === "url") return "url";
  return pipelineUnavailableReason(status, pipelineCount) ? "url" : "pipeline";
}

/** Where the live session is connected, for the bar to show. */
export interface ConnectedTarget {
  source: ConnectSource;
  /** The pipeline the orchestrator reported, when it named one. */
  pipelineName: string | null;
  workerId: string | null;
  url: string;
}

function hostOf(url: string): string {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}

/** Worker ids are long; the tail is what tells two of them apart at a glance. */
function shortWorker(id: string): string {
  return id.length > 12 ? `…${id.slice(-8)}` : id;
}

/** The one-line label for a connected session, e.g. `english › …7f3a91c2`. */
export function describeTarget(target: ConnectedTarget): string {
  if (target.source === "url") return hostOf(target.url);
  const pipeline = target.pipelineName ?? "pipeline";
  return target.workerId ? `${pipeline} › ${shortWorker(target.workerId)}` : `${pipeline} › ${hostOf(target.url)}`;
}

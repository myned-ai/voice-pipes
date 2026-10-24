// Text the Live Graph page shows for workers and for an empty graph.

/** A worker's entry in the selector: its pipeline and its whole id. Worker
 *  ids are names (`worker-greek-parakeet`), so cutting them loses the part
 *  that tells two workers apart. */
export function workerOptionLabel(w: { id: string; pipeline_name?: string | null }): string {
  return w.pipeline_name ? `${w.pipeline_name} (${w.id})` : w.id;
}

export interface EmptyGraphMessage {
  title: string;
  detail: string;
}

/** Why the graph is empty. "No workers" only when there are none: a fleet
 *  with workers whose graph could not be built says so, with the reason the
 *  orchestrator gave when it gave one. */
export function emptyGraphMessage(pipelineError: string | null, workerCount: number): EmptyGraphMessage {
  if (pipelineError) {
    return {
      title: 'Pipeline Unavailable',
      detail: `The pipeline graph could not be built: ${pipelineError}.`,
    };
  }
  if (workerCount > 0) {
    return {
      title: 'No Pipeline Graph',
      detail: 'The selected worker reported no pipeline graph. Pick another worker, or check the orchestrator log.',
    };
  }
  return {
    title: 'No Workers Available',
    detail: 'No workers are connected to the orchestrator. Start a worker to see the live pipeline graph.',
  };
}

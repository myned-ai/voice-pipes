/** One `[[client_tool]]` entry of the connected worker's pipeline config, as
 *  far as the Playground shows it. */
export interface ClientToolInfo {
  name: string;
  description: string;
  /** Parameter names, required ones first. */
  params: string[];
  required: string[];
  /** The agent speaks a confirmation once the client reports the result. */
  speakOnResult: boolean;
}

/** Pull the client tools out of a worker's redacted pipeline config
 *  (`GET /api/v1/workers/{id}` → `pipeline_config.client_tool`). Anything
 *  malformed is skipped rather than shown half-parsed. */
export function parseClientTools(pipelineConfig: unknown): ClientToolInfo[] {
  if (!pipelineConfig || typeof pipelineConfig !== "object") return [];
  const raw = (pipelineConfig as { client_tool?: unknown }).client_tool;
  if (!Array.isArray(raw)) return [];
  return raw.flatMap((t): ClientToolInfo[] => {
    if (!t || typeof t !== "object") return [];
    const tool = t as {
      name?: unknown;
      description?: unknown;
      speak_on_result?: unknown;
      parameters?: { properties?: unknown; required?: unknown };
    };
    if (typeof tool.name !== "string" || !tool.name) return [];
    const props = tool.parameters?.properties;
    const names = props && typeof props === "object" ? Object.keys(props) : [];
    const required = Array.isArray(tool.parameters?.required)
      ? (tool.parameters!.required as unknown[]).filter((r): r is string => typeof r === "string")
      : [];
    return [{
      name: tool.name,
      description: typeof tool.description === "string" ? tool.description : "",
      params: [...names.filter((n) => required.includes(n)), ...names.filter((n) => !required.includes(n))],
      required,
      speakOnResult: tool.speak_on_result === true,
    }];
  });
}

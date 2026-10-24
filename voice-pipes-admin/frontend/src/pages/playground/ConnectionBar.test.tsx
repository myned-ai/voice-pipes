// The bar offers one choice of where a session goes and one button to start
// it. Only the input that belongs to the source in force may be on screen: an
// input that is shown but ignored is what made the old two-button bar
// confusing.

import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { ThemeContext } from "../../contexts/ThemeContext";
import ConnectionBar from "./ConnectionBar";

const pipelines = [
  { name: "nikola_tesla", hash: "aaa", workerCount: 1 },
  { name: "tesla_hr", hash: "bbb", workerCount: 1 },
];

function renderBar(overrides: Partial<React.ComponentProps<typeof ConnectionBar>> = {}) {
  const props = {
    sessionState: "idle" as const,
    pipelineState: null,
    errorMessage: null,
    url: "ws://localhost:8800/ws",
    onUrlChange: vi.fn(),
    source: "pipeline" as const,
    onSourceChange: vi.fn(),
    onStart: vi.fn(),
    onDisconnect: vi.fn(),
    pipelines,
    selectedPipeline: "",
    onPipelineChange: vi.fn(),
    onToggleSettings: vi.fn(),
    showSettings: false,
    onToggleEvents: vi.fn(),
    showEvents: false,
    onToggleTimeline: vi.fn(),
    showTimeline: false,
    sessionMode: "voice" as const,
    onSessionModeChange: vi.fn(),
    onClearConversation: vi.fn(),
    hasConversation: false,
    ...overrides,
  };
  render(
    <ThemeContext.Provider value={{ theme: "dark", isDark: true, toggleTheme: () => {} }}>
      <ConnectionBar {...props} />
    </ThemeContext.Provider>,
  );
  return props;
}

afterEach(cleanup);

describe("ConnectionBar session target", () => {
  it("lists every pipeline and reports the chosen one by name", () => {
    const props = renderBar();
    const select = screen.getByLabelText("Pipeline") as HTMLSelectElement;
    expect(Array.from(select.options).map((o) => o.value)).toEqual(["", "nikola_tesla", "tesla_hr"]);
    fireEvent.change(select, { target: { value: "tesla_hr" } });
    expect(props.onPipelineChange).toHaveBeenCalledWith("tesla_hr");
  });

  it("shows the pipeline list even when there is only one, so the target is never a guess", () => {
    renderBar({ pipelines: [pipelines[0]] });
    expect(screen.getByLabelText("Pipeline")).toBeTruthy();
  });

  it("shows only the input that belongs to the source", () => {
    renderBar({ source: "pipeline" });
    expect(screen.queryByLabelText("Worker URL")).toBeNull();
    cleanup();
    renderBar({ source: "url" });
    expect(screen.getByLabelText("Worker URL")).toBeTruthy();
    expect(screen.queryByLabelText("Pipeline")).toBeNull();
  });

  it("has one start action whatever the source", () => {
    const props = renderBar({ source: "url" });
    fireEvent.click(screen.getByRole("button", { name: "Start session" }));
    expect(props.onStart).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "Dispatch" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Connect" })).toBeNull();
  });

  it("reports a change of source", () => {
    const props = renderBar();
    fireEvent.change(screen.getByLabelText("Connect to"), { target: { value: "url" } });
    expect(props.onSourceChange).toHaveBeenCalledWith("url");
  });

  it("says why a pipeline cannot be chosen, and does not let it be", () => {
    renderBar({ source: "url", pipelineUnavailable: "Orchestrator not reachable" });
    const option = screen.getByRole("option", { name: /orchestrator not reachable/i }) as HTMLOptionElement;
    expect(option.disabled).toBe(true);
  });

  it("says it is looking for a worker while the orchestrator is asked", () => {
    renderBar({ findingWorker: true });
    const button = screen.getByRole("button", { name: /finding a worker/i }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });

  it("replaces the inputs with where the session landed", () => {
    const props = renderBar({
      sessionState: "active",
      target: { source: "pipeline", pipelineName: "tesla_hr", workerId: "worker-0123456789abcdef", url: "ws://10.0.0.4:8800/ws" },
    });
    expect(screen.getByText("tesla_hr › …89abcdef")).toBeTruthy();
    expect(screen.queryByLabelText("Connect to")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "End session" }));
    expect(props.onDisconnect).toHaveBeenCalledTimes(1);
  });
});

describe("ConnectionBar quality tier badge", () => {
  // `full` is the normal state; a badge that is always on is a badge nobody
  // reads, so the indicator only appears when there is something to say.
  it("shows nothing at full quality", () => {
    renderBar({ qualityTier: "full", sessionState: "active" });
    expect(screen.queryByText("Reduced")).toBeNull();
    expect(screen.queryByText("Minimal")).toBeNull();
    expect(screen.queryByText("Critical")).toBeNull();
  });

  it("shows nothing when the server never reports a tier", () => {
    renderBar({ sessionState: "active" });
    expect(screen.queryByText("Reduced")).toBeNull();
  });

  it.each([
    ["reduced", "Reduced"],
    ["minimal", "Minimal"],
    ["critical", "Critical"],
  ] as const)("badges a degraded tier: %s", (tier, label) => {
    renderBar({ qualityTier: tier, sessionState: "active" });
    expect(screen.getByText(label)).toBeTruthy();
  });

  it("explains the tier on hover rather than leaving a bare word", () => {
    renderBar({ qualityTier: "critical", sessionState: "active" });
    const badge = screen.getByText("Critical");
    expect(badge.getAttribute("title")).toMatch(/link/i);
  });

  // The tier describes a live link; showing it after a disconnect would be
  // reporting a condition that no longer exists.
  it("hides the badge once disconnected", () => {
    renderBar({ qualityTier: "critical", sessionState: "idle" });
    expect(screen.queryByText("Critical")).toBeNull();
  });
});

describe("ConnectionBar settings toggle", () => {
  // This button was commented out at some point, which left the jitter buffer
  // and scheduling caps unreachable at runtime — only changeable by rebuilding.
  // The test exists so that cannot happen silently again.
  it("is present and opens the settings panel", () => {
    const props = renderBar({});
    const btn = screen.getByTitle(/Settings/i);
    fireEvent.click(btn);
    expect(props.onToggleSettings).toHaveBeenCalled();
  });
});

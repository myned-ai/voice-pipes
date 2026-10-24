// The Playground lists the connected worker's client tools from the config it
// registered with the orchestrator: a chip that is absent without tools and a
// popover naming each tool, its arguments and whether its result is spoken.

import { describe, it, expect, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { ThemeContext } from "../../../contexts/ThemeContext";
import ClientToolsChip from "../ClientToolsChip";
import { parseClientTools } from "../clientTools";

// The shape `GET /api/v1/workers/{id}` returns as `pipeline_config`.
const pipelineConfig = {
  pipeline: { name: "custom-realtime" },
  client_tool: [
    {
      name: "navigate",
      description: "Navigate the client UI to a named screen.",
      speak_on_result: false,
      parameters: {
        type: "object",
        properties: { screen: { type: "string" } },
        required: ["screen"],
      },
    },
    {
      name: "play_media",
      description: "Start playback of a named media item on the client device.",
      speak_on_result: true,
      parameters: {
        type: "object",
        properties: { volume: { type: "number" }, title: { type: "string" } },
        required: ["title"],
      },
    },
  ],
};

afterEach(cleanup);

describe("parseClientTools", () => {
  it("reads name, description, arguments and the spoken flag", () => {
    expect(parseClientTools(pipelineConfig)).toEqual([
      {
        name: "navigate",
        description: "Navigate the client UI to a named screen.",
        params: ["screen"],
        required: ["screen"],
        speakOnResult: false,
      },
      {
        name: "play_media",
        description: "Start playback of a named media item on the client device.",
        params: ["title", "volume"],
        required: ["title"],
        speakOnResult: true,
      },
    ]);
  });

  it("returns nothing for a config without tools or with malformed entries", () => {
    expect(parseClientTools(undefined)).toEqual([]);
    expect(parseClientTools({ pipeline: {} })).toEqual([]);
    expect(parseClientTools({ client_tool: "navigate" })).toEqual([]);
    expect(parseClientTools({ client_tool: [null, { description: "no name" }, { name: "" }] })).toEqual([]);
  });

  it("keeps a tool without parameters", () => {
    expect(parseClientTools({ client_tool: [{ name: "ping" }] })).toEqual([
      { name: "ping", description: "", params: [], required: [], speakOnResult: false },
    ]);
  });
});

function renderChip(config: unknown) {
  render(
    <ThemeContext.Provider value={{ theme: "dark", isDark: true, toggleTheme: () => {} }}>
      <ClientToolsChip tools={parseClientTools(config)} />
    </ThemeContext.Provider>,
  );
}

describe("ClientToolsChip", () => {
  it("renders nothing when the worker has no client tools", () => {
    renderChip({ pipeline: {} });
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("opens a list of the tools and closes on Escape", () => {
    renderChip(pipelineConfig);
    const chip = screen.getByRole("button", { name: /2 tools/ });
    expect(screen.queryByRole("dialog")).toBeNull();

    fireEvent.click(chip);
    const list = screen.getByRole("dialog", { name: "Client tools" });
    expect(list.textContent).toContain("navigate(screen)");
    expect(list.textContent).toContain("play_media(title, volume?)");
    expect(list.textContent).toContain("silent");
    expect(list.textContent).toContain("spoken");

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("says tool, not tools, for one", () => {
    renderChip({ client_tool: [pipelineConfig.client_tool[0]] });
    expect(screen.getByRole("button", { name: /1 tool$/ })).toBeTruthy();
  });
});

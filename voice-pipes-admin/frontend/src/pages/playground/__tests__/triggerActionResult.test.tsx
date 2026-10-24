// The client-command card's Fail button sent the result box as it stood,
// and the box starts as {"status": "ok"}: an untouched Fail reported a
// failure carrying "ok", and the agent announced the media as playing.

import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { ThemeContext } from "../../../contexts/ThemeContext";
import { TriggerActionBubble } from "../chat/TriggerActionBubble";
import type { TranscriptEntry } from "../session/types";
import { resultTextFor, DEFAULT_COMPLETED_RESULT, DEFAULT_FAILED_RESULT } from "../chat/triggerActionResult";

afterEach(cleanup);

const entry = {
  role: "agent",
  text: "",
  timestamp: 0,
  type: "message",
  pipelineMessage: {
    kind: "trigger_action",
    role: "assistant",
    message_id: "m1",
    payload: { call_id: "call_1", name: "play_media", arguments: { title: "x" }, speak_on_result: true },
  },
} as unknown as TranscriptEntry;

function renderCard() {
  const onResult = vi.fn();
  render(
    <ThemeContext.Provider value={{ theme: "dark", isDark: true, toggleTheme: () => {} }}>
      <TriggerActionBubble entry={entry} onResult={onResult} />
    </ThemeContext.Provider>,
  );
  return onResult;
}

describe("resultTextFor", () => {
  it("uses the outcome's default while the box is untouched", () => {
    expect(resultTextFor("completed", DEFAULT_COMPLETED_RESULT, false)).toBe(DEFAULT_COMPLETED_RESULT);
    expect(resultTextFor("failed", DEFAULT_COMPLETED_RESULT, false)).toBe(DEFAULT_FAILED_RESULT);
  });

  it("sends what the tester typed once they edit it", () => {
    expect(resultTextFor("failed", '{"status": "ok"}', true)).toBe('{"status": "ok"}');
    expect(resultTextFor("completed", "", true)).toBe("");
  });
});

describe("client command card", () => {
  it("sends an error result with an untouched Fail", () => {
    const onResult = renderCard();
    fireEvent.click(screen.getByRole("button", { name: "Fail" }));
    expect(onResult).toHaveBeenCalledWith("call_1", "failed", {
      status: "error",
      error: "failed from the Playground",
    });
  });

  it("sends ok with an untouched Complete", () => {
    const onResult = renderCard();
    fireEvent.click(screen.getByRole("button", { name: "Complete" }));
    expect(onResult).toHaveBeenCalledWith("call_1", "completed", { status: "ok" });
  });

  it("sends the edited result with Fail", () => {
    const onResult = renderCard();
    fireEvent.change(screen.getByRole("textbox"), { target: { value: '{"error": "no such track"}' } });
    fireEvent.click(screen.getByRole("button", { name: "Fail" }));
    expect(onResult).toHaveBeenCalledWith("call_1", "failed", { error: "no such track" });
  });
});

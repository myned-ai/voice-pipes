// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import axios from "axios";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ThemeProvider } from "../../../contexts/ThemeProvider";
import QueueWaitingBanner, { PLAYGROUND_QUEUE_EVENT } from "../QueueWaitingBanner";

vi.mock("axios");

describe("QueueWaitingBanner", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    vi.mocked(axios.get).mockResolvedValue({
      data: {
        data: {
          admission_enabled: true,
          active_sessions: 2,
          leased_sessions: 0,
          capacity_sessions: 3,
          effective_limit: 3,
          queue_depth: 0,
          estimated_wait_s: 0,
        },
      },
    });
  });

  it("renders live capacity strip when admission is enabled", async () => {
    render(
      <ThemeProvider>
        <QueueWaitingBanner sessionState="idle" onLeaveQueue={() => {}} />
      </ThemeProvider>,
    );

    expect(await screen.findByText(/2\/3/)).toBeInTheDocument();
  });

  it("shows waiting room position on queue_status and clears on queue_granted or Leave Queue", async () => {
    const onLeave = vi.fn();
    render(
      <ThemeProvider>
        <QueueWaitingBanner sessionState="connecting" onLeaveQueue={onLeave} />
      </ThemeProvider>,
    );

    act(() => {
      window.dispatchEvent(
        new CustomEvent(PLAYGROUND_QUEUE_EVENT, {
          detail: {
            type: "queue_status",
            ticket_id: "vp_ticket_99",
            position: 2,
            ahead: 1,
            active_sessions: 3,
            capacity_sessions: 3,
            estimated_wait_s: 45,
          },
        }),
      );
    });

    expect(await screen.findByText(/Waiting in Queue — Position #2/)).toBeInTheDocument();
    expect(screen.getByText(/est\. wait ~45s/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Leave Queue/i }));
    expect(onLeave).toHaveBeenCalledTimes(1);
  });
});

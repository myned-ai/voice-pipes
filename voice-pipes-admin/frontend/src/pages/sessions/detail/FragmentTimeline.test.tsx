// FragmentTimeline: renders the track being played with underrun markers,
// barge-in cut, and playhead (non-interactive).

import { describe, it, expect, afterEach, beforeAll } from "vitest";
import { render, cleanup, fireEvent } from "@testing-library/react";
import { createRef } from "react";
import { FragmentTimeline } from "./FragmentTimeline";
import type { FragmentInfo } from "./types";

const frags: FragmentInfo[] = [
  { kind: "tts_frag_000", offset_ms: 0, duration_ms: 500 },
  { kind: "tts_frag_001", offset_ms: 500, duration_ms: 250 },
  { kind: "tts_frag_002", offset_ms: 750, duration_ms: 250 },
];

beforeAll(() => {
  // jsdom: no media pipeline. Back `currentTime` with a plain field and make
  // play() resolve, so seeking can be observed.
  let t = 0;
  Object.defineProperty(HTMLMediaElement.prototype, "currentTime", {
    configurable: true,
    get: () => t,
    set: (v: number) => { t = v; },
  });
  Object.defineProperty(HTMLMediaElement.prototype, "play", {
    configurable: true,
    value: () => Promise.resolve(),
  });
  Object.defineProperty(HTMLMediaElement.prototype, "load", {
    configurable: true,
    value: () => {},
  });
});

afterEach(cleanup);

/** Mount the bar with an <audio> and a 1000 px wide bar so clientX == pct*10. */
function mount(props: Partial<React.ComponentProps<typeof FragmentTimeline>> = {}) {
  const audioRef = createRef<HTMLAudioElement>();
  const utils = render(
    <>
      <audio ref={audioRef} />
      <FragmentTimeline
        fragments={frags}
        heard={{ gaps: [], cutMs: null }}
        mode="server"
        activeFragment={null}
        audioRef={audioRef}
        isDark
        {...props}
      />
    </>,
  );
  const bar = utils.getByRole("img");
  bar.getBoundingClientRect = () => ({ left: 0, top: 0, width: 1000, height: 16, right: 1000, bottom: 16, x: 0, y: 0, toJSON: () => ({}) });
  return { ...utils, bar, audioRef };
}

describe("FragmentTimeline", () => {
  it("draws one segment per fragment and no gap blocks without a report", () => {
    const { container } = mount();
    expect(container.querySelectorAll("[data-seg]")).toHaveLength(3);
    expect(container.querySelectorAll("[data-gap]")).toHaveLength(0);
    expect(container.querySelector("[data-cut]")).toBeNull();
  });

  it("server mode marks underruns as ticks; heard mode draws them as blocks", () => {
    const heard = { gaps: [{ at_audio_ms: 500, silence_ms: 250 }], cutMs: null };
    const server = mount({ heard, mode: "server" });
    const tick = server.container.querySelector("[data-gap]") as HTMLElement;
    expect(tick.style.width).toBe("2px");
    server.unmount();

    const heardView = mount({ heard, mode: "heard" });
    const block = heardView.container.querySelector("[data-gap]") as HTMLElement;
    // 250 of 1250 ms → 20% of the bar.
    expect(block.style.width).toBe("20%");
    expect(heardView.getByText(/1 underrun · 250 ms silent/)).toBeInTheDocument();
  });

  it("draws the barge-in cut and names it in the legend", () => {
    const { container, getByText } = mount({ heard: { gaps: [], cutMs: 600 } });
    expect(container.querySelector("[data-cut]")).not.toBeNull();
    expect(getByText(/barge-in at 600 ms/)).toBeInTheDocument();
  });

  it("clicking or hovering the bar does not seek audio or render a tooltip", () => {
    const { bar, audioRef, queryByText } = mount();
    audioRef.current!.currentTime = 0;
    fireEvent.mouseMove(bar, { clientX: 600 });
    expect(queryByText(/#2 · 500 ms/)).toBeNull();
    fireEvent.click(bar, { clientX: 600 });
    expect(audioRef.current!.currentTime).toBe(0);
  });
});

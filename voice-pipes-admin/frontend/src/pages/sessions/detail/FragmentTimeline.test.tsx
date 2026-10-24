// FragmentTimeline: renders the track being played, seeks on click, and
// switches to selection when the player is on a single fragment.

import { describe, it, expect, afterEach, beforeAll, vi } from "vitest";
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
  const onSelectFragment = vi.fn();
  const utils = render(
    <>
      <audio ref={audioRef} />
      <FragmentTimeline
        fragments={frags}
        heard={{ gaps: [], cutMs: null }}
        mode="server"
        activeFragment={null}
        audioRef={audioRef}
        onSelectFragment={onSelectFragment}
        isDark
        {...props}
      />
    </>,
  );
  const bar = utils.getByRole("img");
  bar.getBoundingClientRect = () => ({ left: 0, top: 0, width: 1000, height: 16, right: 1000, bottom: 16, x: 0, y: 0, toJSON: () => ({}) });
  return { ...utils, bar, audioRef, onSelectFragment };
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

  it("clicking seeks the shared audio to that point on the track and plays", () => {
    const { bar, audioRef } = mount();
    const play = vi.spyOn(audioRef.current!, "play");
    fireEvent.click(bar, { clientX: 600 }); // 60% of 1000 ms
    expect(audioRef.current!.currentTime).toBeCloseTo(0.6, 5);
    expect(play).toHaveBeenCalled();
  });

  it("in single-fragment mode a click selects the fragment under it instead of seeking", () => {
    const { bar, onSelectFragment, audioRef } = mount({ activeFragment: 0 });
    audioRef.current!.currentTime = 0;
    fireEvent.click(bar, { clientX: 800 }); // 800 ms → fragment #3
    expect(onSelectFragment).toHaveBeenCalledWith(2);
    expect(audioRef.current!.currentTime).toBe(0);
  });

  it("draws the barge-in cut and names it in the legend", () => {
    const { container, getByText } = mount({ heard: { gaps: [], cutMs: 600 } });
    expect(container.querySelector("[data-cut]")).not.toBeNull();
    expect(getByText(/barge-in at 600 ms/)).toBeInTheDocument();
  });

  it("hovering a fragment shows its index, offset and duration", () => {
    const { bar, getByText } = mount({ fragmentTexts: { tts_frag_001: "Second sentence here." } });
    fireEvent.mouseMove(bar, { clientX: 600 });
    expect(getByText(/#2 · 500 ms · 250 ms · “Second sentence here.”/)).toBeInTheDocument();
  });
});

// FragmentPlayer density modes: a handful of sentence fragments render as
// labelled chips; hundreds of realtime deltas collapse to a summary line
// with an opt-in stepper, instead of one chip per 40 ms delta.

import { describe, it, expect, afterEach, beforeAll } from "vitest";
import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import { FragmentPlayer, CHIP_THRESHOLD } from "./AudioPlayers";
import { fragmentSnippet, fragmentTextsFor } from "./fragments";
import type { Utterance } from "./types";

beforeAll(() => {
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

const frag = (n: number): [string, string] => {
  const idx = String(n).padStart(3, "0");
  return [`tts_frag_${idx}`, `001_tts_frag_${idx}.wav`];
};
const entriesOf = (count: number): [string, string][] =>
  Array.from({ length: count }, (_, i) => frag(i));

const concatUrl = "/api/v1/sessions/s1/audio/turn/1/tts_concat.wav";
const audioSrc = (container: HTMLElement) => container.querySelector("audio")?.getAttribute("src");

describe("fragmentSnippet", () => {
  it("keeps the first words and marks truncation", () => {
    expect(fragmentSnippet("Welcome, dear visitor! I am Nikola Tesla.")).toBe("Welcome, dear visitor!…");
    expect(fragmentSnippet("How may I")).toBe("How may I");
    expect(fragmentSnippet("   ")).toBe("");
  });
});

describe("fragmentTextsFor", () => {
  it("picks up sentence text the BFF attached under the fragment kind", () => {
    const utt = {
      utterance_id: 1,
      tts_frag_000: "First sentence.",
      tts_frag_001: "   ",
    } as Utterance;
    expect(fragmentTextsFor(utt, entriesOf(3))).toEqual({ tts_frag_000: "First sentence." });
  });
});

describe("FragmentPlayer — sentence fragments (below threshold)", () => {
  it("labels chips with the sentence's first words when text is known", () => {
    render(
      <FragmentPlayer
        entries={entriesOf(2)}
        sessionId="s1"
        concatUrl={concatUrl}
        fragmentTexts={{ tts_frag_000: "Welcome, dear visitor! I am Nikola Tesla." }}
      />,
    );
    expect(screen.getByText("Welcome, dear visitor!…")).toBeInTheDocument();
    // No text for the second fragment → falls back to its number.
    expect(screen.getByText("2")).toBeInTheDocument();
    expect(screen.queryByText(/fragments/)).toBeNull();
  });

  it("clicking a chip plays that fragment", () => {
    const { container } = render(
      <FragmentPlayer entries={entriesOf(3)} sessionId="s1" concatUrl={concatUrl} />,
    );
    expect(audioSrc(container)).toBe(concatUrl);
    fireEvent.click(screen.getByText("2"));
    expect(audioSrc(container)).toContain("001_tts_frag_001.wav");
  });
});

describe("FragmentPlayer — realtime deltas (above threshold)", () => {
  const many = entriesOf(CHIP_THRESHOLD + 1);

  it("collapses to a summary line instead of one chip per fragment", () => {
    render(<FragmentPlayer entries={many} sessionId="s1" concatUrl={concatUrl} />);
    expect(screen.getByText(`${many.length} fragments`)).toBeInTheDocument();
    expect(screen.getByText("all")).toBeInTheDocument();
    // No numbered chips, no stepper until asked for.
    expect(screen.queryByText("1")).toBeNull();
    expect(screen.queryByLabelText("Fragment number")).toBeNull();
  });

  it("renders exactly the threshold as chips, one more as a summary", () => {
    const { unmount } = render(
      <FragmentPlayer entries={entriesOf(CHIP_THRESHOLD)} sessionId="s1" concatUrl={concatUrl} />,
    );
    expect(screen.getByText(String(CHIP_THRESHOLD))).toBeInTheDocument();
    unmount();
    render(<FragmentPlayer entries={many} sessionId="s1" concatUrl={concatUrl} />);
    expect(screen.queryByText(String(CHIP_THRESHOLD))).toBeNull();
  });

  it("'inspect' reveals a stepper that walks fragments and wraps", () => {
    const { container } = render(
      <FragmentPlayer entries={many} sessionId="s1" concatUrl={concatUrl} />,
    );
    fireEvent.click(screen.getByText("inspect"));
    expect(screen.getByLabelText("Fragment number")).toBeInTheDocument();

    // Next from the stitched track lands on the first fragment.
    fireEvent.click(screen.getByLabelText("Next fragment"));
    expect(audioSrc(container)).toContain("001_tts_frag_000.wav");
    expect(screen.getByText("playing #1")).toBeInTheDocument();

    // Previous from the first wraps to the last.
    fireEvent.click(screen.getByLabelText("Previous fragment"));
    expect(audioSrc(container)).toContain(`001_tts_frag_${String(many.length - 1).padStart(3, "0")}.wav`);
  });

  it("typing a fragment number jumps to it, clamped to range", () => {
    const { container } = render(
      <FragmentPlayer entries={many} sessionId="s1" concatUrl={concatUrl} />,
    );
    fireEvent.click(screen.getByText("inspect"));
    const input = screen.getByLabelText("Fragment number");

    fireEvent.change(input, { target: { value: "7" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(audioSrc(container)).toContain("001_tts_frag_006.wav");

    fireEvent.change(input, { target: { value: "9999" } });
    fireEvent.blur(input);
    expect(audioSrc(container)).toContain(`001_tts_frag_${String(many.length - 1).padStart(3, "0")}.wav`);
  });

  it("'all' returns to the stitched track from a fragment", () => {
    const { container } = render(
      <FragmentPlayer entries={many} sessionId="s1" concatUrl={concatUrl} />,
    );
    fireEvent.click(screen.getByText("inspect"));
    fireEvent.click(screen.getByLabelText("Next fragment"));
    expect(audioSrc(container)).not.toBe(concatUrl);
    fireEvent.click(screen.getByText("all"));
    expect(audioSrc(container)).toBe(concatUrl);
  });
});

// Speech that made no turn is shown before the turn that took its id, with
// the recording the capture kept, so a session the recogniser never
// answered is no longer an empty page.

import { describe, it, expect, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { DroppedUtteranceCard } from "./DroppedUtteranceCard";
import { describeDrop } from "./droppedUtterance";
import type { DroppedUtterance } from "./types";

afterEach(cleanup);

const noWords: DroppedUtterance = {
  reason: "empty_transcript",
  text: "",
  had_interim_text: false,
  audio_ms: 1377,
  file: "002_dropped_empty_transcript_mic_raw.wav",
  duration_ms: 1400,
};

describe("DroppedUtteranceCard", () => {
  it("names the reason and plays the kept recording", () => {
    render(<DroppedUtteranceCard drop={noWords} sessionId="sess 1" utteranceId={2} />);
    expect(screen.getByText("Heard, not recognised")).toBeTruthy();
    expect(screen.getByText("before turn 2")).toBeTruthy();
    expect(screen.getByText("The recogniser found no words in 1.4 s of speech")).toBeTruthy();
    const audio = screen.getByLabelText("Recording of dropped speech before turn 2") as HTMLAudioElement;
    expect(audio.getAttribute("src")).toContain("/api/v1/sessions/sess%201/audio/002_dropped_empty_transcript_mic_raw.wav");
  });

  it("says when no recording was kept", () => {
    render(<DroppedUtteranceCard drop={{ ...noWords, file: undefined }} sessionId="s" utteranceId={1} />);
    expect(screen.getByText("no recording kept")).toBeTruthy();
    expect(screen.queryByLabelText(/Recording of dropped speech/)).toBeNull();
  });
});

describe("describeDrop", () => {
  it("words each reason the engine gives", () => {
    expect(describeDrop({ ...noWords, had_interim_text: true })).toBe(
      "The recogniser produced words for 1.4 s of speech and then returned nothing for it",
    );
    expect(describeDrop({ reason: "too_short", text: "Hm", had_interim_text: false, audio_ms: 400 })).toBe(
      "Transcribed as “Hm”, under the minimum length, and discarded",
    );
    expect(describeDrop({ reason: "stt_timeout", text: "", had_interim_text: false, audio_ms: 15200 })).toBe(
      "The recogniser never answered 15.2 s of speech; it was restarted",
    );
    expect(describeDrop({ reason: "other", text: "", had_interim_text: false, audio_ms: null })).toBe(
      "speech made no turn (other)",
    );
  });
});

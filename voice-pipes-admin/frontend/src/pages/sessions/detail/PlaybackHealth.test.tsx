// Playback-fidelity auditing UI: the TurnCardHeader badge and the
// FragmentPlayer as-heard toggle.

import { describe, it, expect, afterEach, beforeAll } from "vitest";
import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import { TurnCardHeader } from "./TurnCardHeader";
import { FragmentPlayer } from "./AudioPlayers";
import type { TurnSummary, Utterance } from "./types";

// jsdom's HTMLMediaElement.play() returns undefined — the player calls
// .play().catch(...) on chip clicks, so give it a real promise.
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

const baseUtt: Utterance = { utterance_id: 1 };

function renderHeader(summary?: TurnSummary, utt: Utterance = baseUtt) {
  return render(
    <TurnCardHeader
      utt={utt}
      displayState="completed"
      turnStatus={undefined}
      summary={summary}
      orchTurn={undefined}
      anomalies={undefined}
      isColdStart={false}
      isRealtime={true}
    />,
  );
}

describe("TurnCardHeader playback-health badge", () => {
  it("renders breaks + silence from summary and a wall/audio ratio tooltip", () => {
    renderHeader({
      audio_underruns: 33,
      total_silence_ms: 9040,
      wall_clock_ms: 25160,
      audio_duration_ms: 16120,
    });
    const badge = screen.getByText(/33 breaks · 9\.0s silent/);
    expect(badge).toBeInTheDocument();
    expect(badge.getAttribute("title")).toContain("reconstructed from client playback timing");
    expect(badge.getAttribute("title")).toContain("1.6× slower than the audio");
  });

  it("renders no badge when there are no underruns", () => {
    renderHeader({ audio_underruns: 0, total_silence_ms: 0 });
    expect(screen.queryByText(/silent/)).toBeNull();
    renderHeader({});
    expect(screen.queryByText(/silent/)).toBeNull();
  });

  it("singular form for one break, no ratio when wall ≈ audio", () => {
    renderHeader({
      audio_underruns: 1,
      total_silence_ms: 300,
      wall_clock_ms: 5300,
      audio_duration_ms: 5000,
    });
    const badge = screen.getByText(/1 break · 0\.3s silent/);
    expect(badge.getAttribute("title")).not.toContain("slower than the audio");
  });

  // The cause of the underruns comes from
  // the client's arrival span — starvation (arrival_rtf > 1) vs jitter (≤ 1).
  it("labels underruns as starvation when audio arrived slower than realtime", () => {
    renderHeader(
      { audio_underruns: 33, total_silence_ms: 9040 },
      { ...baseUtt, playback_health: { underrun_count: 33, total_silence_ms: 9040, audio_ms: 36285, heard_ms: 36000, wall_ms: 45000, gaps: [], arrival_wall_ms: 60000 } },
    );
    const badge = screen.getByText(/33 breaks · 9\.0s silent · starvation/);
    expect(badge.getAttribute("title")).toContain("arrival_rtf 1.65");
    expect(badge.getAttribute("title")).toContain("server output or network throughput");
    expect(badge.textContent).not.toContain("interrupted");
  });

  it("labels underruns as jitter when arrivals kept up on average, and marks flush-path frames interrupted", () => {
    renderHeader(
      { audio_underruns: 2, total_silence_ms: 400 },
      { ...baseUtt, playback_health: { underrun_count: 2, total_silence_ms: 400, audio_ms: 5000, heard_ms: 3000, wall_ms: 3400, gaps: [], arrival_wall_ms: 4000, interrupted: true } },
    );
    const badge = screen.getByText(/2 breaks · 0\.4s silent · jitter · interrupted/);
    expect(badge.getAttribute("title")).toContain("arrival_rtf 0.80");
    expect(badge.getAttribute("title")).toContain("bursty delivery");
    expect(badge.getAttribute("title")).toContain("barge-in/cancel flush");
  });

  it("shows no cause for captures predating arrival_wall_ms, and falls back to playback_health when the summary lacks the counts", () => {
    renderHeader(
      { audio_underruns: 3, total_silence_ms: 1000 },
      { ...baseUtt, playback_health: { underrun_count: 3, total_silence_ms: 1000, audio_ms: 5000, heard_ms: 5000, wall_ms: 6000, gaps: [] } },
    );
    expect(screen.getByText(/3 breaks · 1\.0s silent$/)).toBeInTheDocument();
    cleanup();
    renderHeader(
      {},
      { ...baseUtt, playback_health: { underrun_count: 4, total_silence_ms: 2000, audio_ms: 5000, heard_ms: 5000, wall_ms: 7000, gaps: [], arrival_wall_ms: 2000 } },
    );
    expect(screen.getByText(/4 breaks · 2\.0s silent · jitter/)).toBeInTheDocument();
  });
});

// "output 0.60× realtime" when the
// server's streaming phase was slower than realtime (`output.emit_rtf > 1`).
describe("TurnCardHeader output-throughput badge", () => {
  function uttWithOutput(extras: Record<string, number>, posts: string[] = []): Utterance {
    return {
      ...baseUtt,
      metric_snapshot: {
        utterance_id: 1,
        total_ms: 60000,
        interrupted: false,
        components: [
          { component_id: "output", kind: "output", started_at_ms: 0, extras },
          ...posts.map((name) => ({
            component_id: `audio_post:${name}`, kind: "audio_post", started_at_ms: 0, provider: name,
          })),
        ],
      },
    };
  }

  it("renders 1/emit_rtf as playback speed and blames the chain when emit − source > 0.3", () => {
    renderHeader({}, uttWithOutput({ emit_rtf: 1.67, source_rtf: 0.53, chain_overhead_ms: 41360, audio_output_ms: 36285 }, ["grpc"]));
    const badge = screen.getByText("output 0.60× realtime");
    const tip = badge.getAttribute("title") ?? "";
    expect(tip).toContain("emit_rtf 1.67");
    expect(tip).toContain("source_rtf 0.53");
    expect(tip).toContain("+41360ms");
    expect(tip).toContain("post-processor chain, not the provider");
  });

  it("does not blame a chain on a turn that ran no post-processor", () => {
    renderHeader({}, uttWithOutput({ emit_rtf: 1.3, source_rtf: 0.5, chain_overhead_ms: 4000, audio_output_ms: 6000 }));
    const tip = screen.getByText("output 0.77× realtime").getAttribute("title") ?? "";
    expect(tip).not.toContain("post-processor chain");
    expect(tip).toContain("pacer wait: 4000ms");
    expect(tip).toContain("no post-processor ran");
  });

  it("blames the provider when source_rtf is itself over 1.0", () => {
    renderHeader({}, uttWithOutput({ emit_rtf: 1.3, source_rtf: 1.25, chain_overhead_ms: 200 }));
    const badge = screen.getByText("output 0.77× realtime");
    expect(badge.getAttribute("title")).toContain("provider itself delivers slower than realtime");
  });

  it("renders nothing at or under realtime, or without an output component", () => {
    renderHeader({}, uttWithOutput({ emit_rtf: 1.0, source_rtf: 0.9 }));
    expect(screen.queryByText(/× realtime/)).toBeNull();
    cleanup();
    renderHeader({}, uttWithOutput({ emit_rtf: 0.53, source_rtf: 0.5 }));
    expect(screen.queryByText(/× realtime/)).toBeNull();
    cleanup();
    renderHeader({}, baseUtt);
    expect(screen.queryByText(/× realtime/)).toBeNull();
  });
});

// Anchor provenance badge next to TTFB and
// the S2S provider-endpointing chip.
describe("TurnCardHeader anchor badge and provider endpointing", () => {
  it("renders the anchor badge from ttfb_anchor_source with the short label and explains it in the tooltip", () => {
    renderHeader({ ttfb_ms: 1623, ttfb_anchor_source: "input_transcript_proxy", first_text_ms: 1081 });
    const badge = screen.getByText("anchor: transcript proxy");
    expect(badge.getAttribute("title")).toContain("input-transcript delta");
    expect(badge.getAttribute("title")).toContain("ttfb_anchor_source=input_transcript_proxy");
    const ttfb = screen.getByText("TTFB 1623ms");
    expect(ttfb.getAttribute("title")).toContain("anchor: transcript proxy");
    expect(ttfb.getAttribute("title")).toContain("first transcript delta at 1081 ms");
  });

  it("maps every anchor source to its label; turn_start reads as unanchored", () => {
    renderHeader({ ttfb_ms: 1, ttfb_anchor_source: "turn_start" });
    expect(screen.getByText("anchor: unanchored")).toBeInTheDocument();
    cleanup();
    renderHeader({ ttfb_ms: 500, ttfb_anchor_source: "vp_vad" });
    expect(screen.getByText("anchor: vp vad")).toBeInTheDocument();
    cleanup();
    renderHeader({ ttfb_ms: 500, ttfb_anchor_source: "client_speech_end" });
    expect(screen.getByText("anchor: client vad")).toBeInTheDocument();
    cleanup();
    renderHeader({ ttfb_ms: 500, ttfb_anchor_source: "provider_speech_stopped" });
    expect(screen.getByText("anchor: provider")).toBeInTheDocument();
    cleanup();
    renderHeader({ ttfb_ms: 500, ttfb_anchor_source: "text_submit" });
    expect(screen.getByText("anchor: text")).toBeInTheDocument();
    cleanup();
    renderHeader({ ttfb_ms: 500 });
    expect(screen.queryByText(/^anchor:/)).toBeNull();
  });

  it("shows the provider endpointing chip (rose over 500 ms) and hides the cascade VAD chip when both are present", () => {
    renderHeader({ endpointing_ms: 666, provider_endpointing_ms: 666 });
    const chip = screen.getByText("Endpointing 666ms");
    expect(chip.className).toContain("rose");
    expect(chip.getAttribute("title")).toContain("over budget");
    expect(screen.queryByText(/^VAD /)).toBeNull();
    cleanup();
    renderHeader({ provider_endpointing_ms: 320 });
    expect(screen.getByText("Endpointing 320ms").className).not.toContain("rose");
    cleanup();
    renderHeader({ endpointing_ms: 420 });
    expect(screen.getByText("VAD 420ms")).toBeInTheDocument();
    expect(screen.queryByText(/^Endpointing /)).toBeNull();
  });

  it("names the E2E anchor source in the E2E tooltip", () => {
    renderHeader({ e2e_perceived_ms: 2830, e2e_anchor_source: "client_speech_end" });
    expect(screen.getByText("E2E 2830ms").getAttribute("title")).toContain("anchor: client vad");
  });
});

describe("FragmentPlayer as-heard toggle", () => {
  const entries: [string, string][] = [
    ["tts_frag_000", "001_tts_frag_000.wav"],
    ["tts_frag_001", "001_tts_frag_001.wav"],
  ];
  const concatUrl = "/api/v1/sessions/s1/audio/turn/1/tts_concat.wav";
  const asHeardUrl = "/api/v1/sessions/s1/audio/turn/1/as-heard.wav";

  it("shows Server/As-heard chips only when asHeardUrl is provided", () => {
    const { container, rerender } = render(
      <FragmentPlayer entries={entries} sessionId="s1" concatUrl={concatUrl} />,
    );
    expect(screen.getByText("all")).toBeInTheDocument();
    expect(screen.queryByText("as heard")).toBeNull();
    // Default source is the server concat.
    expect(container.querySelector("audio")?.getAttribute("src")).toBe(concatUrl);

    rerender(
      <FragmentPlayer entries={entries} sessionId="s1" concatUrl={concatUrl} asHeardUrl={asHeardUrl} />,
    );
    expect(screen.getByText("server")).toBeInTheDocument();
    expect(screen.getByText("as heard")).toBeInTheDocument();
    expect(screen.queryByText("all")).toBeNull();
  });

  it("switching to as-heard points the audio element at the as-heard URL", () => {
    const { container } = render(
      <FragmentPlayer entries={entries} sessionId="s1" concatUrl={concatUrl} asHeardUrl={asHeardUrl} />,
    );
    expect(container.querySelector("audio")?.getAttribute("src")).toBe(concatUrl);
    fireEvent.click(screen.getByText("as heard"));
    expect(container.querySelector("audio")?.getAttribute("src")).toBe(asHeardUrl);
    fireEvent.click(screen.getByText("server"));
    expect(container.querySelector("audio")?.getAttribute("src")).toBe(concatUrl);
  });
});

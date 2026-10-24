import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { TurnComponentMetricsTable } from "./TurnComponentMetricsTable";
import {
  makeComponent,
  makeDescriptor,
  makeSnapshot,
} from "../../test/fixtures/turnMetrics";

describe("<TurnComponentMetricsTable>", () => {
  it("renders one section per component in insertion order", () => {
    const snapshot = makeSnapshot();
    render(
      <TurnComponentMetricsTable snapshot={snapshot} descriptors={[]} />,
    );
    const sections = screen.getAllByTestId(/^component-section-/);
    expect(sections.map((s) => s.dataset.testid)).toEqual([
      "component-section-stt",
      "component-section-llm",
      "component-section-tts",
    ]);
  });

  it("falls back to component_id when no descriptor provided", () => {
    const snapshot = makeSnapshot({
      components: [makeComponent({ component_id: "my_translator", kind: "translator" })],
    });
    render(
      <TurnComponentMetricsTable snapshot={snapshot} descriptors={[]} />,
    );
    expect(screen.getByText("my_translator")).toBeTruthy();
  });

  it("uses descriptor.label when descriptor is provided", () => {
    const snapshot = makeSnapshot({
      components: [makeComponent({ component_id: "stt", kind: "stt" })],
    });
    const descriptors = [
      makeDescriptor({ component_id: "stt", kind: "stt", label: "Whisper STT" }),
    ];
    render(
      <TurnComponentMetricsTable snapshot={snapshot} descriptors={descriptors} />,
    );
    expect(screen.getByText("Whisper STT")).toBeTruthy();
  });

  it("renders one row per descriptor field with formatted value", () => {
    const snapshot = makeSnapshot({
      components: [
        makeComponent({
          component_id: "tts",
          kind: "tts",
          extras: { rtf: 0.16, chars: 120 },
        }),
      ],
    });
    const descriptors = [
      makeDescriptor({
        component_id: "tts",
        kind: "tts",
        fields: [
          { key: "rtf", label: "RTF", unit: "ratio" },
          { key: "chars", label: "Chars", unit: "count" },
        ],
      }),
    ];
    render(
      <TurnComponentMetricsTable snapshot={snapshot} descriptors={descriptors} />,
    );
    expect(screen.getByText("RTF")).toBeTruthy();
    expect(screen.getByText("0.16")).toBeTruthy();
    expect(screen.getByText("Chars")).toBeTruthy();
    expect(screen.getByText("120")).toBeTruthy();
  });

  it("shows '-' when an extras key is missing for a descriptor field", () => {
    const snapshot = makeSnapshot({
      components: [
        makeComponent({ component_id: "tts", kind: "tts", extras: {} }),
      ],
    });
    const descriptors = [
      makeDescriptor({
        component_id: "tts",
        kind: "tts",
        fields: [{ key: "rtf", label: "RTF", unit: "ratio" }],
      }),
    ];
    render(
      <TurnComponentMetricsTable snapshot={snapshot} descriptors={descriptors} />,
    );
    expect(screen.getByText("-")).toBeTruthy();
  });

  // `ttfb_anchor_source` is a STRING
  // extra advertised with `unit: "raw"`. A string must render as plain text
  // for any unit — never "NaN", never the "-" placeholder.
  it("renders string extras as plain text regardless of unit", () => {
    const snapshot = makeSnapshot({
      components: [
        makeComponent({
          component_id: "realtime",
          kind: "realtime",
          extras: { ttfb_ms: 1623, ttfb_anchor_source: "input_transcript_proxy", provider: "gemini" },
        }),
      ],
    });
    const descriptors = [
      makeDescriptor({
        component_id: "realtime",
        kind: "realtime",
        fields: [
          { key: "ttfb_ms", label: "TTFB", unit: "ms" },
          { key: "ttfb_anchor_source", label: "Anchor", unit: "raw", health: { role: "diagnostic" } },
          { key: "provider", label: "Provider (ms unit by mistake)", unit: "ms" },
        ],
      }),
    ];
    render(
      <TurnComponentMetricsTable snapshot={snapshot} descriptors={descriptors} />,
    );
    expect(screen.getByText("1623 ms")).toBeTruthy();
    expect(screen.getByText("input_transcript_proxy")).toBeTruthy();
    expect(screen.getByText("gemini")).toBeTruthy();
    expect(screen.queryByText(/NaN/)).toBeNull();
    expect(screen.queryByText("-")).toBeNull();
  });

  it("renders an error banner when component.error is set", () => {
    const snapshot = makeSnapshot({
      components: [
        makeComponent({
          component_id: "tts",
          kind: "tts",
          error: "engine timed out",
        }),
      ],
    });
    render(
      <TurnComponentMetricsTable snapshot={snapshot} descriptors={[]} />,
    );
    const banner = screen.getByTestId("error-banner-tts");
    expect(banner.textContent).toContain("engine timed out");
  });

  it("shows running indicator when finished_at_ms is missing", () => {
    const snapshot = makeSnapshot({
      components: [
        makeComponent({
          component_id: "tts",
          kind: "tts",
          finished_at_ms: undefined,
          latency_ms: undefined,
        }),
      ],
    });
    render(
      <TurnComponentMetricsTable snapshot={snapshot} descriptors={[]} />,
    );
    expect(screen.getByTestId("running-indicator-tts")).toBeTruthy();
  });

  it("renders a friendly empty state when components is empty", () => {
    const snapshot = makeSnapshot({ components: [] });
    render(
      <TurnComponentMetricsTable snapshot={snapshot} descriptors={[]} />,
    );
    expect(
      screen.getByText(/no per-component metrics/i),
    ).toBeTruthy();
  });
});

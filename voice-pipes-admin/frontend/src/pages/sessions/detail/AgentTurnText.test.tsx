/**
 * Component render tests for AgentTurnText.
 *
 * These tests verify the exact visual behaviour shown in the session-detail
 * screenshot: a VAD barge-in turn where the LLM produced a full story but the
 * user interrupted after the opening sentence.
 *
 * Expected render output when interrupted:
 *   "  <heard text>  ✂  <strikethrough unheard text>  "
 */

import { render, screen } from '@testing-library/react';
import { AgentTurnText } from './AgentTurnText';

// ── Shared fixtures (mirror the screenshot values) ────────────────────────────

const STORY_FULL =
  'Once upon a time, in a small village nestled between two mountains, ' +
  'lived a curious girl named Lila. She ' +
  'loved to explore the woods and dreamt of finding a hidden treasure.';

const STORY_RETAINED =
  'Once upon a time, in a small village nestled between two mountains, ' +
  'lived a curious girl named Lila. She';

const VAD_SUMMARY = {
  interrupted: true as const,
  interrupt_trigger: 'server_vad' as const,
  barge_in_rtt_ms: 115,
  last_played_offset_ms: 6113,
};

// ── No interruption ───────────────────────────────────────────────────────────

describe('AgentTurnText — no interruption', () => {
  it('renders the full text inside opening and closing curly quotes', () => {
    render(<AgentTurnText fullText="Hello world" />);
    expect(screen.getByText(/Hello world/)).toBeInTheDocument();
    const p = screen.getByText(/Hello world/).closest('p');
    expect(p?.textContent).toContain('\u201cHello world\u201d');
  });

  it('does not render a scissors icon', () => {
    render(<AgentTurnText fullText="Hello world" />);
    expect(screen.queryByText('✂')).not.toBeInTheDocument();
  });

  it('does not render a strikethrough span', () => {
    const { container } = render(<AgentTurnText fullText="Hello world" />);
    expect(container.querySelector('.line-through')).toBeNull();
  });
});

// ── Interrupted turn (screenshot scenario) ────────────────────────────────────

describe('AgentTurnText — VAD barge-in (screenshot scenario)', () => {
  it('renders the scissors ✂ icon', () => {
    render(
      <AgentTurnText
        fullText={STORY_FULL}
        retainedText={STORY_RETAINED}
        summary={VAD_SUMMARY}
      />,
    );
    expect(screen.getByText('✂')).toBeInTheDocument();
  });

  it('renders the heard portion of the text', () => {
    render(
      <AgentTurnText
        fullText={STORY_FULL}
        retainedText={STORY_RETAINED}
        summary={VAD_SUMMARY}
      />,
    );
    expect(screen.getByText(STORY_RETAINED.trim())).toBeInTheDocument();
  });

  it('renders the unheard portion inside a strikethrough span', () => {
    const { container } = render(
      <AgentTurnText
        fullText={STORY_FULL}
        retainedText={STORY_RETAINED}
        summary={VAD_SUMMARY}
      />,
    );
    const strikeSpan = container.querySelector('.line-through');
    expect(strikeSpan).not.toBeNull();
    expect(strikeSpan?.textContent).toMatch(/loved to explore/);
  });

  it('scissors icon has a tooltip with the RTT from the screenshot', () => {
    render(
      <AgentTurnText
        fullText={STORY_FULL}
        retainedText={STORY_RETAINED}
        summary={VAD_SUMMARY}
      />,
    );
    const scissor = screen.getByText('✂');
    expect(scissor).toHaveAttribute('title', expect.stringContaining('RTT 115ms'));
  });

  it('scissors icon tooltip mentions server VAD trigger', () => {
    render(
      <AgentTurnText
        fullText={STORY_FULL}
        retainedText={STORY_RETAINED}
        summary={VAD_SUMMARY}
      />,
    );
    const scissor = screen.getByText('✂');
    expect(scissor).toHaveAttribute('title', expect.stringContaining('trigger: server VAD'));
  });

  it('scissors icon tooltip includes the playback offset', () => {
    render(
      <AgentTurnText
        fullText={STORY_FULL}
        retainedText={STORY_RETAINED}
        summary={VAD_SUMMARY}
      />,
    );
    const scissor = screen.getByText('✂');
    // 6113ms → 6.1s
    expect(scissor).toHaveAttribute('title', expect.stringContaining('Interrupted at 6.1s'));
  });

  it('the heard part is NOT struck through', () => {
    const { container } = render(
      <AgentTurnText
        fullText={STORY_FULL}
        retainedText={STORY_RETAINED}
        summary={VAD_SUMMARY}
      />,
    );
    const strikeSpan = container.querySelector('.line-through');
    expect(strikeSpan?.textContent).not.toContain('Once upon a time');
  });

  it('the full tooltip string matches the expected format', () => {
    render(
      <AgentTurnText
        fullText={STORY_FULL}
        retainedText={STORY_RETAINED}
        summary={VAD_SUMMARY}
      />,
    );
    expect(screen.getByText('✂')).toHaveAttribute(
      'title',
      'Interrupted at 6.1s · RTT 115ms · trigger: server VAD',
    );
  });
});

// ── Interrupted turn WITHOUT retained_text (common when no barge_in_rtt event) ──

describe('AgentTurnText — interrupted but no retained_text', () => {
  it('still renders a scissors ✂ icon at end of text', () => {
    render(
      <AgentTurnText
        fullText={STORY_FULL}
        summary={VAD_SUMMARY}
      />,
    );
    expect(screen.getByText('✂')).toBeInTheDocument();
  });

  it('shows the full text (no strikethrough — cutoff unknown)', () => {
    render(
      <AgentTurnText
        fullText={STORY_FULL}
        summary={VAD_SUMMARY}
      />,
    );
    const { container } = render(
      <AgentTurnText
        fullText={STORY_FULL}
        summary={VAD_SUMMARY}
      />,
    );
    // full text visible
    expect(screen.getAllByText(/Once upon a time/).length).toBeGreaterThan(0);
    // no strikethrough since we don't know the cutoff
    expect(container.querySelector('.line-through')).toBeNull();
  });

  it('shows scissors when client_interrupted is true and no retained_text', () => {
    render(
      <AgentTurnText
        fullText="The quick brown fox jumps over the lazy dog."
        summary={{ client_interrupted: true, last_played_offset_ms: 2000 }}
      />,
    );
    expect(screen.getByText('✂')).toBeInTheDocument();
  });

  it('does NOT show scissors when turn is not interrupted and no retained_text', () => {
    render(
      <AgentTurnText
        fullText="Hello world"
        summary={{ interrupted: false }}
      />,
    );
    expect(screen.queryByText('✂')).not.toBeInTheDocument();
  });
});

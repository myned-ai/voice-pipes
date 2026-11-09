import { describe, it, expect, afterEach, beforeAll } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';
import { TurnCard } from './TurnCard';
import type { Utterance } from './types';

beforeAll(() => {
  Object.defineProperty(HTMLMediaElement.prototype, 'play', {
    configurable: true,
    value: () => Promise.resolve(),
  });
  Object.defineProperty(HTMLMediaElement.prototype, 'load', {
    configurable: true,
    value: () => {},
  });
});

afterEach(cleanup);

describe('TurnCard TTS input inspector', () => {
  it('renders TTS input button when tts_directives are present and expands on click', () => {
    const utt: Utterance = {
      utterance_id: 1,
      llm_output: {
        text: 'Hello! Check your SQL database.',
        spoken_text: '<break time="300ms"/> Hello! Check your sequel database.',
        tts_directives: [
          { kind: 'inline_tag', raw: '<break time="300ms"/>' },
          { kind: 'lexicon', from: 'SQL', to: 'sequel' },
        ],
      },
    };

    render(<TurnCard utt={utt} sessionId="test-session" />);

    const button = screen.getByText(/TTS input · 1 directive · 1 lexicon/);
    expect(button).toBeInTheDocument();

    expect(screen.queryByText('Spoken text sent to TTS')).toBeNull();

    fireEvent.click(button);

    expect(screen.getByText('Spoken text sent to TTS')).toBeInTheDocument();
    expect(screen.getByText('<break time="300ms"/> Hello! Check your sequel database.')).toBeInTheDocument();
    expect(screen.getByText('<break time="300ms"/>')).toBeInTheDocument();
    expect(screen.getByText('SQL')).toBeInTheDocument();
    expect(screen.getByText('sequel')).toBeInTheDocument();
  });

  it('renders amber warning badge when stripped_directives is non-empty and displays reason', () => {
    const utt: Utterance = {
      utterance_id: 2,
      llm_output: {
        text: 'Hello world',
        spoken_text: 'Hello world',
        stripped_directives: [
          { raw: '[whispers]', reason: "not in passthrough_tags for provider 'kokoro'" },
        ],
      },
    };

    render(<TurnCard utt={utt} sessionId="test-session" />);

    const badge = screen.getByText(/· 1 stripped ⚠/);
    expect(badge).toBeInTheDocument();

    const button = badge.closest('button')!;
    fireEvent.click(button);

    expect(screen.getByText('Stripped Directives')).toBeInTheDocument();
    expect(screen.getByText('[whispers]')).toBeInTheDocument();
    expect(screen.getByText("not in passthrough_tags for provider 'kokoro'")).toBeInTheDocument();
  });

  it('hides TTS input toggle when spoken_text is identical/absent and no directives exist', () => {
    const utt: Utterance = {
      utterance_id: 3,
      llm_output: {
        text: 'Hello world',
        spoken_text: 'Hello world',
      },
    };

    render(<TurnCard utt={utt} sessionId="test-session" />);

    expect(screen.queryByText(/TTS input/)).toBeNull();
  });

  it('renders inline spoken text when spoken_text differs from display text', () => {
    const utt: Utterance = {
      utterance_id: 4,
      llm_output: {
        text: 'The URL for Google is https://www.google.com.',
        spoken_text: 'The URL for Google is google dot com.',
      },
    };

    render(<TurnCard utt={utt} sessionId="test-session" />);

    expect(screen.getByText('Spoken:')).toBeInTheDocument();
    expect(screen.getByText(/google dot com\./)).toBeInTheDocument();
  });
});

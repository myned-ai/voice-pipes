import { describe, it, expect, vi, afterEach, beforeAll } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import axios from 'axios';
import Playground from '../Playground';
import type { PlaygroundSession } from '../usePlaygroundSession';
import { ThemeProvider } from '../../../contexts/ThemeProvider';
import { setMockViewport } from '../../../test/viewport';

beforeAll(() => {
  Element.prototype.scrollIntoView = vi.fn();
});

vi.mock('axios', () => ({
  default: {
    get: vi.fn().mockResolvedValue({ data: { data: { groups: [] } } }),
  },
}));

vi.mock('../extensionRegistry', () => ({
  EXTENSION_READY_TIMEOUT_MS: 5000,
  loadPlaygroundExtensionsOnce: vi.fn().mockResolvedValue({ loaded: [], failed: [] }),
  pendingExtensions: vi.fn().mockReturnValue([]),
  waitForExtensionsReady: vi.fn().mockResolvedValue([]),
}));

function makeSessionStub(): PlaygroundSession {
  return {
    sessionState: 'idle',
    pipelineState: null,
    errorMessage: null,
    transcript: [],
    events: [],
    stateTimeline: [],
    turnMetrics: [],
    metricDescriptors: [],
    rttMs: null,
    micActive: false,
    micLevel: 0,
    playbackLevel: 0,
    qualityTier: 'full',
    connect: vi.fn(),
    disconnect: vi.fn(),
    startMic: vi.fn(),
    stopMic: vi.fn(),
    sendText: vi.fn(),
    cancelAgent: vi.fn(),
    clearConversation: vi.fn(),
    flushLocalAudio: vi.fn(),
    sendClientToolResult: vi.fn(),
  } as unknown as PlaygroundSession;
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('Playground responsive side panels', () => {
  it('allows EventLog and StateTimeline side-by-side on desktop (1440px)', () => {
    setMockViewport(1440);
    render(
      <ThemeProvider>
        <Playground session={makeSessionStub()} />
      </ThemeProvider>,
    );

    fireEvent.click(screen.getByRole('button', { name: 'Events' }));
    fireEvent.click(screen.getByRole('button', { name: 'Timeline' }));

    expect(screen.queryByTestId('playground-mobile-overlay')).toBeNull();
    expect(screen.getByRole('heading', { name: 'Events' })).toBeInTheDocument();
    expect(screen.getByText('State Timeline')).toBeInTheDocument();
  });

  it('renders EventLog and StateTimeline as mutually exclusive overlay drawers on mobile (360px)', () => {
    setMockViewport(360);
    void axios;
    render(
      <ThemeProvider>
        <Playground session={makeSessionStub()} />
      </ThemeProvider>,
    );

    fireEvent.click(screen.getByRole('button', { name: 'Events' }));
    expect(screen.getByTestId('playground-mobile-overlay')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Events' })).toBeInTheDocument();
    expect(screen.queryByText('State Timeline')).toBeNull();

    // Toggling Timeline closes Events and switches the overlay drawer
    fireEvent.click(screen.getByRole('button', { name: 'Timeline' }));
    expect(screen.getByTestId('playground-mobile-overlay')).toBeInTheDocument();
    expect(screen.getByText('State Timeline')).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'Events' })).toBeNull();
  });
});

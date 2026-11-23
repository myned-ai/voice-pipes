import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, cleanup, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import axios from 'axios';
import SessionsList from '../SessionsList';
import { ThemeProvider } from '../../../contexts/ThemeProvider';
import { setMockViewport } from '../../../test/viewport';

vi.mock('axios', () => ({
  default: {
    get: vi.fn(),
  },
}));

const mockedGet = vi.mocked(axios.get);

const SAMPLE_SESSIONS = [
  {
    id: 'sess-101-abcdef',
    status: 'completed',
    pipeline_name: 'support_agent',
    stt_provider: 'parakeet',
    tts_provider: 'kokoro',
    turn_count: 4,
    started_at: '2026-11-22T20:00:00Z',
    ended_at: '2026-11-22T20:01:30Z',
    tags: [],
  },
  {
    id: 'sess-102-ghijkl',
    status: 'error',
    pipeline_name: 'sales_bot',
    stt_provider: 'nemotron',
    tts_provider: 'piper',
    turn_count: 1,
    started_at: '2026-11-22T19:00:00Z',
    ended_at: '2026-11-22T19:00:25Z',
    tags: ['benchmark'],
  },
];

function setupMocks() {
  mockedGet.mockImplementation((url: string) => {
    if (url.includes('/api/v1/pipelines')) {
      return Promise.resolve({ data: { data: { groups: [] } } });
    }
    return Promise.resolve({
      data: {
        data: {
          items: SAMPLE_SESSIONS,
          total: SAMPLE_SESSIONS.length,
          page: 1,
          page_size: 25,
        },
      },
    });
  });
}

function renderList() {
  return render(
    <ThemeProvider>
      <MemoryRouter>
        <SessionsList />
      </MemoryRouter>
    </ThemeProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('SessionsList responsive views', () => {
  it('renders full 7-column desktop table at 1440px', async () => {
    setMockViewport(1440);
    setupMocks();
    renderList();

    await waitFor(() => {
      expect(screen.getByText('sess-101-abcdef')).toBeInTheDocument();
    });

    for (const col of ['Status', 'Session', 'Pipeline', 'Providers', 'Turns', 'Started', 'Length']) {
      expect(screen.getByText(col)).toBeInTheDocument();
    }
    expect(screen.queryByTestId('sessions-mobile-cards')).toBeNull();
    expect(screen.getAllByRole('button', { name: 'Copy session ID' })).toHaveLength(2);
  });

  it('renders compact session cards on mobile (360px) and portrait tablet (768px)', async () => {
    for (const width of [360, 768]) {
      setMockViewport(width);
      setupMocks();
      renderList();

      const cards = await screen.findByTestId('sessions-mobile-cards');
      expect(cards).toBeInTheDocument();
      expect(screen.queryByText('Providers')).toBeNull();

      expect(screen.getByText('support_agent')).toBeInTheDocument();
      expect(screen.getByText('4 turns')).toBeInTheDocument();
      expect(screen.getByText('1 turn')).toBeInTheDocument();
      expect(screen.getByText('benchmark')).toBeInTheDocument();

      const link = screen.getByText('sess-101-abcdef').closest('a');
      expect(link).toHaveAttribute('href', '/sessions/sess-101-abcdef');

      cleanup();
    }
  });
});

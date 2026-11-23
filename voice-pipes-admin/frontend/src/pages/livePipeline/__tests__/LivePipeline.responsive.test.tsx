import { describe, it, expect, vi, afterEach, beforeAll } from 'vitest';
import { render, screen, fireEvent, cleanup, act } from '@testing-library/react';
import LivePipeline from '../../LivePipeline';
import { ThemeProvider } from '../../../contexts/ThemeProvider';
import { setMockViewport } from '../../../test/viewport';

beforeAll(() => {
  class MockEventSource {
    onmessage: ((ev: MessageEvent) => void) | null = null;
    onerror: ((ev: Event) => void) | null = null;
    close() {}
  }
  Object.defineProperty(globalThis, 'EventSource', {
    configurable: true,
    writable: true,
    value: MockEventSource,
  });
});

vi.mock('axios', () => ({
  default: {
    get: vi.fn().mockResolvedValue({ data: { data: null } }),
    patch: vi.fn(),
    isAxiosError: () => false,
  },
}));

vi.mock('../../../lib/adminAuth', () => ({
  checkAuth: vi.fn().mockResolvedValue({
    auth_required: false,
    authenticated: true,
    is_operator: true,
  }),
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('LivePipeline responsive orientation', () => {
  it('automatically selects vertical orientation on narrow mobile screens (360px)', () => {
    setMockViewport(360);
    render(
      <ThemeProvider>
        <LivePipeline />
      </ThemeProvider>,
    );

    // When vertical orientation is active, the toggle button offers to switch to "Horizontal"
    const toggleBtn = screen.getByTitle('Switch to Horizontal Graph Layout');
    expect(toggleBtn).toHaveTextContent('Horizontal');
  });

  it('automatically selects horizontal orientation on wide screens (1440px) and auto-switches to vertical when resized to mobile (360px)', () => {
    setMockViewport(1440);
    render(
      <ThemeProvider>
        <LivePipeline />
      </ThemeProvider>,
    );

    // Wide screen defaults to horizontal (button offers to switch to "Vertical")
    expect(screen.getByTitle('Switch to Vertical Graph Layout')).toHaveTextContent('Vertical');

    // Even if user manually toggles twice on desktop (locking an explicit desktop preference),
    // resizing to narrow mobile (360px) automatically selects vertical orientation.
    fireEvent.click(screen.getByTitle('Switch to Vertical Graph Layout'));
    expect(screen.getByTitle('Switch to Horizontal Graph Layout')).toHaveTextContent('Horizontal');
    fireEvent.click(screen.getByTitle('Switch to Horizontal Graph Layout'));
    expect(screen.getByTitle('Switch to Vertical Graph Layout')).toHaveTextContent('Vertical');

    act(() => {
      setMockViewport(360);
    });

    expect(screen.getByTitle('Switch to Horizontal Graph Layout')).toHaveTextContent('Horizontal');
  });
});

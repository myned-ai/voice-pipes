import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import App from '../App';
import { setMockViewport } from '../test/viewport';

vi.mock('../components/AuthGate', () => ({
  AuthGate: ({ children }: { children: ReactNode }) => <>{children}</>,
}));

vi.mock('../lib/features', () => ({
  DEFAULT_ADMIN_FEATURES: { benchmarks: true, test_suites: true },
  fetchAdminFeatures: vi.fn().mockResolvedValue({ benchmarks: true, test_suites: true }),
}));

vi.mock('../pages/playground/usePlaygroundSession', () => ({
  usePlaygroundSession: () => ({
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
  }),
}));

vi.mock('../pages/Dashboard', () => ({ default: () => <div data-testid="page-dashboard">Dashboard Page</div> }));
vi.mock('../pages/LivePipeline', () => ({ default: () => <div data-testid="page-live">Live Page</div> }));
vi.mock('../pages/sessions/SessionsList', () => ({ default: () => <div data-testid="page-sessions">Sessions Page</div> }));
vi.mock('../pages/sessions/SessionDetail', () => ({ default: () => <div data-testid="page-session-detail">Session Detail</div> }));
vi.mock('../pages/playground/Playground', () => ({ default: () => <div data-testid="page-playground">Playground Page</div> }));

afterEach(() => {
  cleanup();
  window.history.pushState({}, '', '/');
});

describe('App responsive shell', () => {
  it('renders expanded desktop sidebar at 1440px and supports collapse/expand without mobile drawer', async () => {
    setMockViewport(1440);
    render(<App />);

    const primaryNav = await screen.findByRole('navigation', { name: 'Primary' });
    expect(primaryNav).toBeInTheDocument();
    expect(screen.getByText('Session History')).toBeInTheDocument();
    expect(screen.queryByRole('dialog', { name: 'Navigation drawer' })).toBeNull();

    const collapseBtn = screen.getByTitle('Collapse Sidebar');
    fireEvent.click(collapseBtn);
    expect(screen.getByTitle('Expand Sidebar')).toBeInTheDocument();
    expect(screen.queryByText('Session History')).toBeNull();

    fireEvent.click(screen.getByTitle('Expand Sidebar'));
    expect(screen.getByTitle('Collapse Sidebar')).toBeInTheDocument();
    expect(screen.getByText('Session History')).toBeInTheDocument();
  });

  it('defaults tablet sidebar to collapsed icon rail at 768px and allows manual expand', async () => {
    setMockViewport(768);
    render(<App />);

    expect(await screen.findByTitle('Expand Sidebar')).toBeInTheDocument();
    expect(screen.queryByText('Session History')).toBeNull();

    fireEvent.click(screen.getByTitle('Expand Sidebar'));
    expect(screen.getByTitle('Collapse Sidebar')).toBeInTheDocument();
    expect(screen.getByText('Session History')).toBeInTheDocument();
  });

  it('opens mobile drawer at 360px and closes on navigation, Escape, and backdrop click', async () => {
    setMockViewport(360);
    render(<App />);

    const openBtn = await screen.findByRole('button', { name: 'Open navigation' });
    expect(openBtn).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('dialog', { name: 'Navigation drawer' })).toBeNull();

    // Open drawer
    fireEvent.click(openBtn);
    expect(openBtn).toHaveAttribute('aria-expanded', 'true');
    const drawer = screen.getByRole('dialog', { name: 'Navigation drawer' });
    expect(drawer).toBeInTheDocument();

    // Navigate via link inside drawer -> navigates & auto-closes drawer
    const mobileNav = screen.getByRole('navigation', { name: 'Mobile Primary' });
    const sessionsLink = mobileNav.querySelector('a[href="/sessions"]');
    expect(sessionsLink).not.toBeNull();
    fireEvent.click(sessionsLink!);

    await waitFor(() => {
      expect(screen.getByTestId('page-sessions')).toBeInTheDocument();
      expect(screen.queryByRole('dialog', { name: 'Navigation drawer' })).toBeNull();
    });

    // Re-open and close with Escape
    fireEvent.click(screen.getByRole('button', { name: 'Open navigation' }));
    expect(screen.getByRole('dialog', { name: 'Navigation drawer' })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog', { name: 'Navigation drawer' })).toBeNull();

    // Re-open and close via backdrop click
    fireEvent.click(screen.getByRole('button', { name: 'Open navigation' }));
    fireEvent.click(screen.getByTestId('mobile-drawer-backdrop'));
    expect(screen.queryByRole('dialog', { name: 'Navigation drawer' })).toBeNull();
  });
});

import { useState, useEffect } from "react";
import type { ComponentType } from "react";
import { BrowserRouter as Router, Routes, Route, NavLink, Navigate, useLocation } from "react-router-dom";
import { Mic, History, Activity, Menu, Gamepad2, Sun, Moon, BarChart3, FlaskConical, ClipboardList, X } from "lucide-react";
import type { LucideProps } from "lucide-react";
import LivePipeline from "./pages/LivePipeline";
import SessionsList from "./pages/sessions/SessionsList";
import SessionDetail from "./pages/sessions/SessionDetail";
import Playground from "./pages/playground/Playground";
import Dashboard from "./pages/Dashboard";
import BenchmarksList from "./pages/benchmarks/BenchmarksList";
import BenchmarkDetail from "./pages/benchmarks/BenchmarkDetail";
import BenchmarkCompare from "./pages/benchmarks/BenchmarkCompare";
import ExperimentEditor from "./pages/benchmarks/ExperimentEditor";
import SuitesList from "./pages/benchmarks/SuitesList";
import SuiteDetail from "./pages/benchmarks/SuiteDetail";
import SuiteEditor from "./pages/benchmarks/SuiteEditor";
import { useTheme } from "./contexts/ThemeContext";
import { ThemeProvider } from "./contexts/ThemeProvider";
import { usePlaygroundSession } from "./pages/playground/usePlaygroundSession";
import { AuthGate } from "./components/AuthGate";
import { DEFAULT_ADMIN_FEATURES, fetchAdminFeatures, type AdminFeatures } from "./lib/features";
import { BP_LG, useMediaQuery } from "./lib/useMediaQuery";

/* Navigation is grouped by intent so the sidebar has a clear visual hierarchy
   instead of one long flat list: "Monitor" = observing live/historical traffic,
   "Build & Test" = authoring and evaluating pipelines.  Driving it from data
   keeps every item's styling identical (no copy-paste drift) and makes adding
   a route a one-line change. */
type NavItem = {
  to: string;
  label: string;
  icon: ComponentType<LucideProps>;
  end?: boolean;
  feature?: keyof AdminFeatures;
};
type NavGroup = { heading: string; items: NavItem[] };

const NAV_GROUPS: NavGroup[] = [
  {
    heading: "Monitor",
    items: [
      { to: "/", label: "Dashboard", icon: BarChart3, end: true },
      { to: "/live", label: "Live Graph", icon: Activity },
      { to: "/sessions", label: "Session History", icon: History },
    ],
  },
  {
    heading: "Build & Test",
    items: [
      { to: "/playground", label: "Playground", icon: Gamepad2 },
      { to: "/benchmarks", label: "Benchmarks", icon: FlaskConical, feature: "benchmarks" },
      { to: "/suites", label: "Test Suites", icon: ClipboardList, feature: "test_suites" },
    ],
  },
];

function SidebarLink({ item, expanded, onNavigate }: { item: NavItem; expanded: boolean; onNavigate?: () => void }) {
  const { isDark } = useTheme();
  const Icon = item.icon;
  return (
    <NavLink
      to={item.to}
      end={item.end}
      title={item.label}
      onClick={onNavigate}
      className={({ isActive }) =>
        `touch-target group relative flex items-center gap-3 p-3 rounded-lg text-sm font-medium transition-colors ${
          isActive
            ? isDark
              ? "bg-slate-800 text-white"
              : "bg-gray-100 text-gray-900"
            : isDark
              ? "text-slate-400 hover:bg-slate-800/50 hover:text-white"
              : "text-gray-500 hover:bg-gray-100 hover:text-gray-900"
        } ${expanded ? "" : "justify-center"}`
      }
    >
      {({ isActive }) => (
        <>
          {/* Active accent bar — a standard wayfinding cue for the current page. */}
          <span
            aria-hidden="true"
            className={`absolute left-0 top-1/2 -translate-y-1/2 h-5 w-0.5 rounded-full bg-indigo-400 transition-opacity ${
              isActive ? "opacity-100" : "opacity-0"
            }`}
          />
          <Icon size={20} className="shrink-0" />
          {expanded && <span className="whitespace-nowrap">{item.label}</span>}
        </>
      )}
    </NavLink>
  );
}

export default function App() {
  return (
    <ThemeProvider>
      {/* Nothing below mounts (and so nothing calls the BFF) until the
          gate has a working ADMIN_UI_TOKEN or the BFF reports auth is off. */}
      <AuthGate>
        <AppContent />
      </AuthGate>
    </ThemeProvider>
  );
}

function AppContent() {
  const [features, setFeatures] = useState<AdminFeatures>(DEFAULT_ADMIN_FEATURES);
  const { isDark } = useTheme();
  // Session is instantiated here (above the router) so it survives navigation.
  const playgroundSession = usePlaygroundSession();

  useEffect(() => {
    let active = true;
    void fetchAdminFeatures().then((f) => {
      if (active) setFeatures(f);
    });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    document.documentElement.style.backgroundColor = isDark ? '#0f172a' : '#f8fafc';
    document.documentElement.setAttribute('data-theme', isDark ? 'dark' : 'light');
  }, [isDark]);

  return (
    <Router>
      <AppShell features={features} playgroundSession={playgroundSession} />
    </Router>
  );
}

function AppShell({
  features,
  playgroundSession,
}: {
  features: AdminFeatures;
  playgroundSession: ReturnType<typeof usePlaygroundSession>;
}) {
  const isLg = useMediaQuery(BP_LG);
  const [sidebarPref, setSidebarPref] = useState<boolean | null>(null);
  const isSidebarOpen = sidebarPref ?? isLg;
  const { pathname } = useLocation();
  const [drawerOpenAtPath, setDrawerOpenAtPath] = useState<string | null>(null);
  const isMobileDrawerOpen = drawerOpenAtPath === pathname;
  const openMobileDrawer = () => setDrawerOpenAtPath(pathname);
  const closeMobileDrawer = () => setDrawerOpenAtPath(null);
  const { isDark, toggleTheme } = useTheme();

  useEffect(() => {
    if (!isMobileDrawerOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setDrawerOpenAtPath(null);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [isMobileDrawerOpen]);

  const visibleNavGroups = NAV_GROUPS.map((group) => ({
    ...group,
    items: group.items.filter((item) => !item.feature || features[item.feature]),
  })).filter((group) => group.items.length > 0);

  return (
    <div className={`flex flex-col md:flex-row h-dvh w-full overflow-hidden font-sans transition-colors duration-300 ${isDark ? 'bg-slate-900 text-white' : 'bg-gray-50 text-gray-900'}`}>

      {/* Mobile Top Bar (< md) */}
      <header
        className={`flex md:hidden items-center justify-between min-h-14 px-4 pt-[env(safe-area-inset-top)] border-b shrink-0 z-20 ${
          isDark ? 'bg-slate-950 border-slate-800 text-white' : 'bg-gray-200 border-gray-300 text-gray-900'
        }`}
      >
        <div className="flex items-center gap-2.5">
          <button
            type="button"
            onClick={openMobileDrawer}
            aria-label="Open navigation"
            aria-expanded={isMobileDrawerOpen}
            aria-controls="mobile-nav-drawer"
            className={`touch-target p-2 -ml-2 rounded-md transition-colors ${
              isDark ? 'text-slate-300 hover:text-white hover:bg-slate-800' : 'text-gray-600 hover:text-gray-900 hover:bg-gray-100'
            }`}
          >
            <Menu size={20} />
          </button>
          <span className="text-base font-bold flex items-center gap-2 whitespace-nowrap">
            <Mic size={18} className="text-indigo-400 shrink-0" />
            Voice-Pipes
          </span>
        </div>
        <button
          type="button"
          onClick={toggleTheme}
          aria-label={`Switch to ${isDark ? 'Light' : 'Dark'} Mode`}
          title={`Switch to ${isDark ? 'Light' : 'Dark'} Mode`}
          className={`touch-target p-2 -mr-2 rounded-md transition-colors ${
            isDark ? 'text-slate-300 hover:text-white hover:bg-slate-800' : 'text-gray-600 hover:text-gray-900 hover:bg-gray-100'
          }`}
        >
          {isDark ? <Sun size={18} className="text-amber-400" /> : <Moon size={18} />}
        </button>
      </header>

      {/* Mobile Slide-Over Navigation Drawer (< md) */}
      {isMobileDrawerOpen && (
        <div
          id="mobile-nav-drawer"
          role="dialog"
          aria-modal="true"
          aria-label="Navigation drawer"
          className="fixed inset-0 z-50 flex md:hidden"
        >
          <div
            data-testid="mobile-drawer-backdrop"
            className="fixed inset-0 bg-black/60 backdrop-blur-xs"
            onClick={closeMobileDrawer}
          />
          <div
            className={`relative z-10 flex flex-col w-72 max-w-[82vw] h-[100svh] px-5 py-5 gap-6 overflow-y-auto shadow-2xl ${
              isDark ? 'bg-slate-950 border-r border-slate-800 text-white' : 'bg-gray-200 border-r border-gray-300 text-gray-900'
            }`}
          >
            <div className="flex items-center justify-between">
              <div className="flex flex-col">
                <span className="text-lg font-bold flex items-center gap-2">
                  <Mic size={20} className="text-indigo-400 shrink-0" /> Voice-Pipes
                </span>
                <span className="text-[10px] text-slate-500 uppercase tracking-[0.1em] font-bold ml-7">
                  Admin Console
                </span>
              </div>
              <button
                type="button"
                onClick={closeMobileDrawer}
                aria-label="Close navigation"
                className={`touch-target p-2 rounded-md ${
                  isDark ? 'text-slate-400 hover:text-white hover:bg-slate-800' : 'text-gray-500 hover:text-gray-900 hover:bg-gray-100'
                }`}
              >
                <X size={20} />
              </button>
            </div>
            <nav aria-label="Mobile Primary" className="flex flex-col gap-4">
              {visibleNavGroups.map((group) => (
                <div key={group.heading} className="flex flex-col gap-1">
                  <span className={`px-3 mb-1 text-[10px] uppercase tracking-[0.1em] font-bold ${isDark ? 'text-slate-600' : 'text-gray-400'}`}>
                    {group.heading}
                  </span>
                  {group.items.map((item) => (
                    <SidebarLink key={item.to} item={item} expanded={true} onNavigate={closeMobileDrawer} />
                  ))}
                </div>
              ))}
            </nav>
          </div>
        </div>
      )}

      {/* Collapsible Sidebar (tablet & desktop >= md) */}
      <div className={`hidden md:flex flex-col transition-all duration-300 ease-in-out ${isSidebarOpen ? 'w-64 px-6 py-6' : 'w-20 px-3 py-6'} gap-8 relative z-20 ${isDark ? 'border-r border-slate-800 bg-slate-950' : 'border-r border-gray-300 bg-gray-200'}`}>
        
        {/* Header area with Logo and Hamburger */}
        <div className={`flex items-center ${isSidebarOpen ? 'justify-between' : 'justify-center'} h-10`}>
          {isSidebarOpen && (
            <div className="flex flex-col">
              <h1 className="text-lg font-bold flex items-center gap-2 whitespace-nowrap overflow-hidden">
                <Mic size={20} className="text-indigo-400 shrink-0" /> Voice-Pipes
              </h1>
              <span className="text-[10px] text-slate-500 uppercase tracking-[0.1em] font-bold ml-7 whitespace-nowrap">Admin Console</span>
            </div>
          )}
          <button 
            onClick={() => setSidebarPref(!isSidebarOpen)}
            className={`touch-target p-2 rounded-md transition-colors shrink-0 ${isSidebarOpen ? '' : 'absolute top-6'} ${isDark ? 'text-slate-400 hover:text-white hover:bg-slate-800' : 'text-gray-500 hover:text-gray-900 hover:bg-gray-100'}`}
            title={isSidebarOpen ? "Collapse Sidebar" : "Expand Sidebar"}
          >
            <Menu size={20} />
          </button>
        </div>

        {/* Navigation Links */}
        <nav aria-label="Primary" className={`flex flex-col gap-4 ${isSidebarOpen ? '' : 'mt-8'}`}>
          {visibleNavGroups.map((group, gi) => (
            <div key={group.heading} className="flex flex-col gap-1">
              {isSidebarOpen ? (
                <span className={`px-3 mb-1 text-[10px] uppercase tracking-[0.1em] font-bold ${isDark ? 'text-slate-600' : 'text-gray-400'}`}>
                  {group.heading}
                </span>
              ) : (
                gi > 0 && <div className={`mx-3 mb-1 border-t ${isDark ? 'border-slate-800' : 'border-gray-300'}`} />
              )}
              {group.items.map((item) => (
                <SidebarLink key={item.to} item={item} expanded={isSidebarOpen} />
              ))}
            </div>
          ))}
        </nav>

        {/* Spacer to push theme toggle to bottom */}
        <div className="flex-1" />

        {/* Theme Toggle */}
        <button
          onClick={toggleTheme}
          className={`touch-target flex items-center gap-3 p-3 rounded-lg text-sm font-medium transition-colors ${isDark ? 'text-slate-400 hover:bg-slate-800/50 hover:text-white' : 'text-gray-500 hover:bg-gray-100 hover:text-gray-900'} ${isSidebarOpen ? '' : 'justify-center'}`}
          title={`Switch to ${isDark ? 'Light' : 'Dark'} Mode`}
        >
          {isDark ? <Sun size={20} className="shrink-0 text-amber-400" /> : <Moon size={20} className="shrink-0" />}
          {isSidebarOpen && <span className="whitespace-nowrap">{isDark ? 'Light Mode' : 'Dark Mode'}</span>}
        </button>
      </div>

      {/* Main Content Area */}
      <div className={`flex-1 relative overflow-hidden flex flex-col w-full min-h-0 transition-colors duration-300 ${isDark ? 'bg-slate-900' : 'bg-gray-50'}`}>
        <Routes>
          <Route path="/" element={<Dashboard />} />
          <Route path="/live" element={<LivePipeline refinedDesign={features.live_graph_redesign} />} />
          <Route path="/playground" element={<Playground session={playgroundSession} />} />
          {features.benchmarks ? (
            <>
              <Route path="/benchmarks" element={<BenchmarksList />} />
              <Route path="/benchmarks/new" element={<ExperimentEditor />} />
              <Route path="/benchmarks/compare" element={<BenchmarkCompare />} />
              <Route path="/benchmarks/:id" element={<BenchmarkDetail />} />
            </>
          ) : (
            <Route path="/benchmarks/*" element={<Navigate to="/" replace />} />
          )}
          {features.test_suites ? (
            <>
              <Route path="/suites" element={<SuitesList />} />
              <Route path="/suites/new" element={<SuiteEditor editMode={false} />} />
              <Route path="/suites/:id/edit" element={<SuiteEditor editMode={true} />} />
              <Route path="/suites/:id" element={<SuiteDetail />} />
            </>
          ) : (
            <Route path="/suites/*" element={<Navigate to="/" replace />} />
          )}
          <Route path="/sessions" element={<SessionsList />} />
          <Route path="/sessions/:id" element={<SessionDetail />} />
        </Routes>
      </div>
    </div>
  );
}

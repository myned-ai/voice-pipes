import { useState, useEffect } from "react";
import type { ComponentType } from "react";
import { BrowserRouter as Router, Routes, Route, NavLink } from "react-router-dom";
import { Mic, History, Activity, Menu, Gamepad2, Sun, Moon, BarChart3, FlaskConical, ClipboardList } from "lucide-react";
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
      { to: "/benchmarks", label: "Benchmarks", icon: FlaskConical },
      { to: "/suites", label: "Test Suites", icon: ClipboardList },
    ],
  },
];

function SidebarLink({ item, expanded }: { item: NavItem; expanded: boolean }) {
  const { isDark } = useTheme();
  const Icon = item.icon;
  return (
    <NavLink
      to={item.to}
      end={item.end}
      title={item.label}
      className={({ isActive }) =>
        `group relative flex items-center gap-3 p-3 rounded-lg text-sm font-medium transition-colors ${
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
  const [isSidebarOpen, setIsSidebarOpen] = useState(true);
  const { isDark, toggleTheme } = useTheme();
  // Session is instantiated here (above the router) so it survives navigation.
  const playgroundSession = usePlaygroundSession();

  useEffect(() => {
    document.documentElement.style.backgroundColor = isDark ? '#0f172a' : '#f8fafc';
    document.documentElement.setAttribute('data-theme', isDark ? 'dark' : 'light');
  }, [isDark]);

  return (
    <Router>
      <div className={`flex h-screen w-full overflow-hidden font-sans transition-colors duration-300 ${isDark ? 'bg-slate-900 text-white' : 'bg-gray-50 text-gray-900'}`}>
        
        {/* Collapsible Sidebar */}
        <div className={`flex flex-col transition-all duration-300 ease-in-out ${isSidebarOpen ? 'w-64 px-6 py-6' : 'w-20 px-3 py-6'} gap-8 relative z-20 ${isDark ? 'border-r border-slate-800 bg-slate-950' : 'border-r border-gray-300 bg-gray-200'}`}>
          
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
              onClick={() => setIsSidebarOpen(!isSidebarOpen)}
              className={`p-2 rounded-md transition-colors shrink-0 ${isSidebarOpen ? '' : 'absolute top-6'} ${isDark ? 'text-slate-400 hover:text-white hover:bg-slate-800' : 'text-gray-500 hover:text-gray-900 hover:bg-gray-100'}`}
              title={isSidebarOpen ? "Collapse Sidebar" : "Expand Sidebar"}
            >
              <Menu size={20} />
            </button>
          </div>

          {/* Navigation Links */}
          <nav aria-label="Primary" className={`flex flex-col gap-4 ${isSidebarOpen ? '' : 'mt-8'}`}>
            {NAV_GROUPS.map((group, gi) => (
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
            className={`flex items-center gap-3 p-3 rounded-lg text-sm font-medium transition-colors ${isDark ? 'text-slate-400 hover:bg-slate-800/50 hover:text-white' : 'text-gray-500 hover:bg-gray-100 hover:text-gray-900'} ${isSidebarOpen ? '' : 'justify-center'}`}
            title={`Switch to ${isDark ? 'Light' : 'Dark'} Mode`}
          >
            {isDark ? <Sun size={20} className="shrink-0 text-amber-400" /> : <Moon size={20} className="shrink-0" />}
            {isSidebarOpen && <span className="whitespace-nowrap">{isDark ? 'Light Mode' : 'Dark Mode'}</span>}
          </button>
        </div>

        {/* Main Content Area */}
        <div className={`flex-1 relative overflow-hidden flex flex-col w-full h-full transition-colors duration-300 ${isDark ? 'bg-slate-900' : 'bg-gray-50'}`}>
          <Routes>
            <Route path="/" element={<Dashboard />} />
            <Route path="/live" element={<LivePipeline />} />
            <Route path="/playground" element={<Playground session={playgroundSession} />} />
            <Route path="/benchmarks" element={<BenchmarksList />} />
            <Route path="/benchmarks/new" element={<ExperimentEditor />} />
            <Route path="/benchmarks/compare" element={<BenchmarkCompare />} />
            <Route path="/benchmarks/:id" element={<BenchmarkDetail />} />
            <Route path="/suites" element={<SuitesList />} />
            <Route path="/suites/new" element={<SuiteEditor editMode={false} />} />
            <Route path="/suites/:id/edit" element={<SuiteEditor editMode={true} />} />
            <Route path="/suites/:id" element={<SuiteDetail />} />
            <Route path="/sessions" element={<SessionsList />} />
            <Route path="/sessions/:id" element={<SessionDetail />} />
          </Routes>
        </div>
      </div>
    </Router>
  );
}

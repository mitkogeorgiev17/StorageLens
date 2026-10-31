/**
 * The frame every screen slots into: sidebar navigation, a header, the active
 * screen inside an error boundary, and a status line for the scan session.
 *
 * Screens without a component yet are placeholders with honest empty states
 * until their own issues land (Overview #22, Large Files #23, Cleanup #29+,
 * Apps #36+, Duplicates #39+). Each issue adds its component to `COMPONENTS`.
 */

import { Component, type ComponentType, type ErrorInfo, type ReactNode } from "react";

import MapScreen from "./routes/Map";
import { formatBytes, formatCount, formatDuration } from "./lib/format";
import { ROUTE_IDS, type RouteId, type ScanState, useAppStore } from "./store";

interface ScreenInfo {
  readonly label: string;
  readonly title: string;
  /** What the screen will show, in one sentence. */
  readonly purpose: string;
  /** What to do when there is nothing to show yet. */
  readonly empty: string;
}

const SCREENS: Readonly<Record<RouteId, ScreenInfo>> = {
  overview: {
    label: "Overview",
    title: "Overview",
    purpose: "Capacity, the biggest consumers, and how much can safely be freed.",
    empty: "No scan yet. Scan a volume to see where the space went.",
  },
  map: {
    label: "Map",
    title: "Storage map",
    purpose: "A zoomable treemap of every folder, sized by space on disk.",
    empty: "The map appears after the first scan.",
  },
  "large-files": {
    label: "Large Files",
    title: "Large files",
    purpose: "Every file, biggest first, filterable by type and age.",
    empty: "Large files are listed after the first scan.",
  },
  apps: {
    label: "Apps",
    title: "Apps",
    purpose: "Installed apps with their full footprint and when they were last used.",
    empty: "App inventory is not built yet.",
  },
  cleanup: {
    label: "Cleanup",
    title: "Cleanup",
    purpose: "Caches and leftovers that are safe to remove, with Trash and undo.",
    empty: "Cleanup recommendations are not built yet.",
  },
  duplicates: {
    label: "Duplicates",
    title: "Duplicates",
    purpose: "Identical files, grouped, with a keep-one choice per group.",
    empty: "Duplicate detection is not built yet.",
  },
  history: {
    label: "History",
    title: "History",
    purpose: "Past scans and everything moved to the Trash, with undo.",
    empty: "No history yet. Finished scans and cleanups will be listed here.",
  },
};

/**
 * Keeps one broken screen from blanking the window.
 *
 * The one class component in the UI: React has no hook-based error boundary,
 * and a dependency for one would break AGENTS.md §2.3 instead of §8.
 */
class ErrorBoundary extends Component<
  { readonly resetKey: string; readonly children: ReactNode },
  { readonly error: Error | null }
> {
  override state: { readonly error: Error | null } = { error: null };

  static getDerivedStateFromError(error: unknown): { error: Error } {
    return { error: error instanceof Error ? error : new Error(String(error)) };
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    // Local console only — there is no telemetry (AGENTS.md §2.2).
    console.error("screen crashed", error, info.componentStack);
  }

  override componentDidUpdate(previous: { readonly resetKey: string }): void {
    // Navigating away gives the next screen a fresh start.
    if (previous.resetKey !== this.props.resetKey && this.state.error !== null) {
      this.setState({ error: null });
    }
  }

  override render(): ReactNode {
    const { error } = this.state;
    if (error === null) {
      return this.props.children;
    }
    return (
      <section className="empty" role="alert">
        <h2>This screen hit an error</h2>
        <p className="empty-text">{error.message}</p>
        <button type="button" className="button" onClick={() => this.setState({ error: null })}>
          Try again
        </button>
      </section>
    );
  }
}

function Sidebar() {
  const route = useAppStore((state) => state.route);
  const navigate = useAppStore((state) => state.navigate);

  return (
    <nav className="sidebar" aria-label="Screens">
      <div className="brand">StorageLens</div>
      <ul className="nav">
        {ROUTE_IDS.map((id) => (
          <li key={id}>
            <button
              type="button"
              className="nav-item"
              aria-current={route === id ? "page" : undefined}
              onClick={() => navigate(id)}
            >
              {SCREENS[id].label}
            </button>
          </li>
        ))}
      </ul>
      <p className="sidebar-note">Offline. Nothing leaves this machine.</p>
    </nav>
  );
}

/** Screens that are built. The rest show their placeholder. */
const COMPONENTS: Partial<Record<RouteId, ComponentType>> = {
  map: MapScreen,
};

function Screen({ id }: { readonly id: RouteId }) {
  const Built = COMPONENTS[id];
  if (Built !== undefined) {
    return <Built />;
  }
  const screen = SCREENS[id];
  return (
    <section className="empty" aria-labelledby="empty-title">
      <h2 id="empty-title">{screen.empty}</h2>
      <p className="empty-text">{screen.purpose}</p>
    </section>
  );
}

function scanStatus(scan: ScanState): string {
  switch (scan.kind) {
    case "idle":
      return "No scan yet";
    case "scanning":
      return `Scanning ${scan.root} · ${formatCount(scan.progress.files)} files · ${formatBytes(
        scan.progress.bytesAlloc,
      )} · ${formatDuration(scan.progress.elapsedMs)}`;
    case "done": {
      const { summary } = scan;
      const gaps =
        summary.skippedDirs > 0 ? ` · ${formatCount(summary.skippedDirs)} folders not scanned` : "";
      return `${summary.root} · ${formatBytes(summary.bytesAlloc)} in ${formatCount(
        summary.files,
      )} files · ${formatDuration(summary.elapsedMs)}${gaps}`;
    }
    case "failed":
      return `Scan failed: ${scan.message}`;
  }
}

function StatusBar() {
  const scan = useAppStore((state) => state.scan);
  return (
    <footer className="status" role="status" aria-live="polite" data-kind={scan.kind}>
      {scanStatus(scan)}
    </footer>
  );
}

export default function App() {
  const route = useAppStore((state) => state.route);

  return (
    <div className="app">
      <Sidebar />
      <div className="main">
        <header className="header">
          <h1>{SCREENS[route].title}</h1>
        </header>
        <main className="content" data-route={route}>
          <ErrorBoundary resetKey={route}>
            <Screen id={route} />
          </ErrorBoundary>
        </main>
        <StatusBar />
      </div>
    </div>
  );
}

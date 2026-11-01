/**
 * The frame every screen slots into: sidebar navigation, a header, the active
 * screen inside an error boundary, and a status line for the scan session.
 *
 * Screens without a component yet are placeholders with honest empty states
 * until their own issues land. Each issue adds its component to `COMPONENTS`.
 *
 * Above every screen: the last move to the Trash with its undo, and the
 * rescan that follows it (#97), so neither is lost by switching screens.
 */

import {
  Component,
  useState,
  type ComponentType,
  type ErrorInfo,
  type ReactNode,
} from "react";

import logo from "./assets/logo.png";
import NavIcon from "./components/NavIcon";
import SearchBar from "./components/SearchBar";
import AppsScreen from "./routes/Apps";
import CleanupScreen from "./routes/Cleanup";
import LargeFilesScreen from "./routes/LargeFiles";
import MapScreen from "./routes/Map";
import OverviewScreen from "./routes/Overview";
import StartupScreen from "./routes/Startup";
import { formatBytes, formatCount, formatDuration } from "./lib/format";
import { api } from "./lib/api";
import { afterUndo, stopRefresh } from "./lib/rescan";
import { ResultPanel } from "./routes/Cleanup";
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
    empty: "No scan yet. Start one from the Overview.",
  },
  map: {
    label: "Map",
    title: "Storage map",
    purpose: "A zoomable treemap of every folder, sized by space on disk.",
    empty: "The map appears after the first scan. Start one from the Overview.",
  },
  "large-files": {
    label: "Large Files",
    title: "Large files",
    purpose: "Every file, biggest first, filterable by type and age.",
    empty: "Large files are listed after the first scan. Start one from the Overview.",
  },
  apps: {
    label: "Apps",
    title: "Apps",
    purpose: "Installed apps with their full footprint and when they were last used.",
    empty: "Installed apps are listed here.",
  },
  startup: {
    label: "Startup",
    title: "Startup",
    purpose: "What starts at login, with a switch for each.",
    empty: "Startup items are listed here.",
  },
  cleanup: {
    label: "Cleanup",
    title: "Cleanup",
    purpose: "Caches, leftovers and duplicate files to remove, with Trash and undo.",
    empty: "Cleanup suggestions appear after a scan.",
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
      <div className="brand">
        <img className="brand-logo" src={logo} alt="" width={24} height={24} />
        StorageLens
      </div>
      <ul className="nav">
        {ROUTE_IDS.map((id) => (
          <li key={id}>
            <button
              type="button"
              className="nav-item"
              aria-current={route === id ? "page" : undefined}
              onClick={() => navigate(id)}
            >
              <NavIcon id={id} />
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
  overview: OverviewScreen,
  map: MapScreen,
  "large-files": LargeFilesScreen,
  apps: AppsScreen,
  startup: StartupScreen,
  cleanup: CleanupScreen,
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
      const ended = summary.status === "cancelled" ? " · cancelled" : "";
      return `${summary.root} · ${formatBytes(summary.bytesAlloc)} in ${formatCount(
        summary.files,
      )} files · ${formatDuration(summary.elapsedMs)}${gaps}${ended}`;
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

/** The refresh after a cleanup, or why the data on screen is from before it. */
function Freshness() {
  const refresh = useAppStore((state) => state.refresh);
  const staleReason = useAppStore((state) => state.staleReason);
  if (refresh !== null) {
    return (
      <div className="banner banner-refresh" role="status" aria-live="polite">
        <p>
          <strong>Refreshing after the cleanup.</strong> Rescanning ·{" "}
          {formatCount(refresh.progress.files)} files · {formatBytes(refresh.progress.bytesAlloc)}. What
          you see is from before it until the scan finishes.
        </p>
        <button
          type="button"
          className="button button-secondary"
          disabled={refresh.scanId === null}
          onClick={stopRefresh}
        >
          Stop
        </button>
      </div>
    );
  }
  if (staleReason !== null) {
    return (
      <div className="banner" role="note">
        <p>{staleReason} Sizes may include what was moved to the Trash.</p>
      </div>
    );
  }
  return null;
}

/** The last move to the Trash, with its undo, on every screen. */
function LastTrash() {
  const last = useAppStore((state) => state.lastTrash);
  const dismiss = useAppStore((state) => state.trashDismissed);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // A clean move needs no popup: the files are in the Trash, which can put
  // them back. Only something not moved, or a warning, is worth showing.
  const clean =
    last !== null &&
    last.undone === null &&
    last.result.warning === null &&
    last.result.items.every((item) => item.status === "trashed");
  if (last === null || clean) {
    return null;
  }
  const undo = () => {
    setBusy(true);
    setError(null);
    api
      .undoDeletion(last.result.manifestId)
      .then(afterUndo)
      .catch((cause: unknown) => setError(cause instanceof Error ? cause.message : String(cause)))
      .finally(() => setBusy(false));
  };
  return (
    <>
      {error !== null ? (
        <p className="map-notice" role="alert">
          {error}
        </p>
      ) : null}
      <ResultPanel result={last.result} undone={last.undone} busy={busy} onUndo={undo} onDismiss={dismiss} />
    </>
  );
}

/**
 * "New scan" in the header, once there is a scan to replace: rescans the
 * whole startup disk straight away, no picker. Other targets stay on the
 * empty-state picker.
 */
function NewScan() {
  const scanKind = useAppStore((state) =>
    // One scan at a time: no new scan while a refresh runs (Stop it first).
    state.refresh === null ? state.scan.kind : "refreshing",
  );
  const scanStarted = useAppStore((state) => state.scanStarted);
  const navigate = useAppStore((state) => state.navigate);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (scanKind !== "done") {
    return null;
  }
  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      const targets = await api.listScanTargets();
      const disk = targets.find((target) => target.startup) ?? targets.find((target) => target.kind === "volume");
      if (disk === undefined) {
        throw new Error("No disk found to scan.");
      }
      scanStarted(await api.startScan(disk.path), disk.path);
      navigate("overview");
    } catch (cause: unknown) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      {error !== null ? (
        <span className="header-error" role="alert">
          {error}
        </span>
      ) : null}
      <button
        type="button"
        className="button button-secondary button-icon"
        title="Scan the whole disk again"
        disabled={busy}
        onClick={() => void start()}
      >
        <svg viewBox="0 0 24 24" width={16} height={16} fill="none" stroke="currentColor" strokeWidth={1.75} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M20 12a8 8 0 1 1-2.3-5.6" />
          <path d="M20 4v5h-5" />
        </svg>
        {busy ? "Starting…" : "New scan"}
      </button>
    </>
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
          <div className="header-actions">
            <SearchBar />
            <NewScan />
          </div>
        </header>
        <main className="content" data-route={route}>
          <ErrorBoundary resetKey={route}>
            <Freshness />
            <LastTrash />
            <Screen id={route} />
          </ErrorBoundary>
        </main>
        <StatusBar />
      </div>
    </div>
  );
}

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
  useRef,
  useState,
  type ComponentType,
  type ErrorInfo,
  type ReactNode,
} from "react";

import ScanPicker from "./components/ScanPicker";
import SearchBar from "./components/SearchBar";
import AppsScreen from "./routes/Apps";
import CleanupScreen from "./routes/Cleanup";
import CollectionsScreen from "./routes/Collections";
import DuplicatesScreen from "./routes/Duplicates";
import LargeFilesScreen from "./routes/LargeFiles";
import MapScreen from "./routes/Map";
import OverviewScreen from "./routes/Overview";
import { formatBytes, formatCount, formatDuration } from "./lib/format";
import { openCollection } from "./lib/collectionActions";
import { api } from "./lib/api";
import { afterUndo, stopRefresh } from "./lib/rescan";
import { ResultPanel } from "./routes/Cleanup";
import { ROUTE_IDS, type RouteId, type ScanState, useAppStore } from "./store";

/** Collections listed in the sidebar before "N more". */
const SIDEBAR_COLLECTIONS = 8;

/** Saved collections under their nav item: one click opens one on the Map. */
function SidebarCollections() {
  const collections = useAppStore((state) => state.collections);
  const scanId = useAppStore((state) =>
    state.scan.kind === "done" ? state.scan.summary.scanId : null,
  );
  const navigate = useAppStore((state) => state.navigate);
  if (collections === null || collections.length === 0) {
    return null;
  }
  const shown = collections.slice(0, SIDEBAR_COLLECTIONS);
  const more = collections.length - shown.length;
  return (
    <ul className="nav-sub" aria-label="Saved collections">
      {shown.map((collection) => (
        <li key={collection.id}>
          <button
            type="button"
            className="nav-subitem"
            title={collection.name}
            onClick={() => {
              if (scanId === null || collection.query === null) {
                navigate("collections");
                return;
              }
              // A collection that cannot open here explains why on its screen.
              openCollection(collection, scanId).catch(() => navigate("collections"));
            }}
          >
            {collection.name}
          </button>
        </li>
      ))}
      {more > 0 ? (
        <li>
          <button type="button" className="nav-subitem nav-more" onClick={() => navigate("collections")}>
            {more} more…
          </button>
        </li>
      ) : null}
    </ul>
  );
}

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
  collections: {
    label: "Collections",
    title: "Collections",
    purpose: "Saved Map views that re-run on every scan.",
    empty: "No collections yet. Save a view from the Map.",
  },
  apps: {
    label: "Apps",
    title: "Apps",
    purpose: "Installed apps with their full footprint and when they were last used.",
    empty: "Installed apps are listed here.",
  },
  cleanup: {
    label: "Cleanup",
    title: "Cleanup",
    purpose: "Caches and leftovers that are safe to remove, with Trash and undo.",
    empty: "Cleanup suggestions appear after a scan.",
  },
  duplicates: {
    label: "Duplicates",
    title: "Duplicates",
    purpose: "Identical files, grouped, with a keep-one choice per group.",
    empty: "Duplicate files are listed here after a search."
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
            {id === "collections" ? <SidebarCollections /> : null}
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
  collections: CollectionsScreen,
  apps: AppsScreen,
  cleanup: CleanupScreen,
  duplicates: DuplicatesScreen,
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
  if (last === null) {
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
 * "New scan" in the header, once there is a scan to replace. A native modal
 * `<dialog>`: it traps focus and closes on Esc; focus goes back to the button.
 */
function NewScan() {
  const scanKind = useAppStore((state) =>
    // One scan at a time: no new scan while a refresh runs (Stop it first).
    state.refresh === null ? state.scan.kind : "refreshing",
  );
  const dialogRef = useRef<HTMLDialogElement | null>(null);
  const openerRef = useRef<HTMLButtonElement | null>(null);
  const [open, setOpen] = useState(false);

  if (scanKind !== "done") {
    return null;
  }
  const close = () => dialogRef.current?.close();
  return (
    <>
      <button
        ref={openerRef}
        type="button"
        className="button button-secondary"
        onClick={() => {
          setOpen(true);
          dialogRef.current?.showModal();
        }}
      >
        New scan
      </button>
      <dialog
        ref={dialogRef}
        className="dialog"
        aria-labelledby="new-scan-title"
        onClose={() => {
          setOpen(false);
          openerRef.current?.focus();
        }}
      >
        <div className="dialog-heading">
          <h2 id="new-scan-title">New scan</h2>
          <button type="button" className="dialog-close" aria-label="Close" onClick={close}>
            ×
          </button>
        </div>
        {/* Mounted only while open, so targets and access are re-checked each time. */}
        {open ? <ScanPicker onStarted={close} /> : null}
      </dialog>
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

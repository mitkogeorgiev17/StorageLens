import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import App from "./App";
import { api, bridgeScanEvents, toSummary } from "./lib/api";
import { installHistory } from "./lib/history";
import { installRescanQueue, rescanWhenPermitted } from "./lib/rescan";
import { useAppStore } from "./store";
import "./styles.css";

const container = document.getElementById("root");
if (container === null) {
  throw new Error("missing #root element");
}

/**
 * Connect the store to the backend, but only inside the Tauri webview: `npm run
 * dev` in a plain browser has no backend, and the shell should still render.
 */
async function connectBackend(): Promise<void> {
  if (!("__TAURI_INTERNALS__" in window)) {
    return;
  }
  const store = useAppStore.getState();
  await bridgeScanEvents(store);

  // Reopen on the last finished scan, so the app is useful at once, then
  // rescan it in the background: it may be days old (#171). Not before the
  // permissions are in (#175).
  const latest = await api.latestScan();
  const summary = latest === null ? null : toSummary(latest);
  if (summary !== null) {
    store.scanFinished(summary);
    rescanWhenPermitted();
  }
}

connectBackend().catch((error: unknown) => {
  // Local console only — there is no telemetry (AGENTS.md §2.2).
  console.error("could not connect to the backend", error);
});

// Back/forward over screens and the Map's view (#26).
installHistory();
installRescanQueue();

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

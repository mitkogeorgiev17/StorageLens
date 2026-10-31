/**
 * Back/forward for the screen and the Map's view (#26), through the window's
 * own history: every change pushes a hash (`lib/mapView.ts`), and `popstate`
 * restores it. No router dependency — the store stays the source of truth.
 *
 * The webview has no toolbar, so back/forward come from the platform's keys:
 * ⌘[ / ⌘] are handled here on macOS (WKWebView has no default for them), and
 * on Windows WebView2 handles Alt+← / Alt+→ and the mouse's back/forward
 * buttons itself.
 */

import { ROUTE_IDS, useAppStore, type RouteId } from "../store.ts";
import { fromHash, toHash } from "./mapView.ts";

const IS_MAC = typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

function isRoute(value: string | null): value is RouteId {
  return (ROUTE_IDS as readonly (string | null)[]).includes(value);
}

/** Start mirroring; returns a function that stops. */
export function installHistory(): () => void {
  let restoring = false;

  const restore = () => {
    const { route, view } = fromHash(window.location.hash);
    restoring = true;
    try {
      useAppStore.getState().restore(isRoute(route) ? route : "overview", view);
    } finally {
      restoring = false;
    }
  };

  const initial = useAppStore.getState();
  window.history.replaceState(null, "", toHash(initial.route, initial.mapView));

  const unsubscribe = useAppStore.subscribe((state, previous) => {
    if (restoring || (state.route === previous.route && state.mapView === previous.mapView)) {
      return;
    }
    const hash = toHash(state.route, state.mapView);
    if (hash !== window.location.hash) {
      window.history.pushState(null, "", hash);
    }
  });

  const onKeyDown = (event: KeyboardEvent) => {
    if (!IS_MAC || !event.metaKey || event.altKey || event.ctrlKey || event.shiftKey) {
      return;
    }
    if (event.key === "[") {
      event.preventDefault();
      window.history.back();
    } else if (event.key === "]") {
      event.preventDefault();
      window.history.forward();
    }
  };

  window.addEventListener("popstate", restore);
  window.addEventListener("keydown", onKeyDown);
  return () => {
    unsubscribe();
    window.removeEventListener("popstate", restore);
    window.removeEventListener("keydown", onKeyDown);
  };
}

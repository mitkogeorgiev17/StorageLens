/**
 * The Map screen (PLAN §13): the treemap of one folder at a time, a few levels
 * deep, with a breadcrumb back to the scan root.
 *
 * | Action            | Mouse                    | Keyboard                      |
 * | ----------------- | ------------------------ | ----------------------------- |
 * | Inspect           | hover (tooltip)          | arrows (details line)         |
 * | Zoom into folder  | click                    | Enter                         |
 * | Go up             | breadcrumb               | Esc, ⌘↑ (macOS) / Alt+↑       |
 * | Reveal            | double-click             | ⌘/Ctrl+Enter                  |
 * | Preview           | Space over the item      | Space on the selection        |
 *
 * The toolbar switches the grouping (Folder / Type / App / Age / Size /
 * Cleanliness) and sets filter chips (#26). The view lives in the store and
 * the URL hash, so it survives other screens and back/forward. The plain
 * folder view reads `get_tree`; every other view reads `get_grouping`, which
 * returns the same tree shape, so the treemap draws them all unchanged. While
 * a new view loads, the old one stays on screen (no flash).
 *
 * In the Folder view the breadcrumb is the location. Switching to another
 * grouping inside a folder limits that view to the folder (a removable chip);
 * switching back to Folder opens it again.
 *
 * State is per scan: the screen is keyed by scan id, so a rescan starts over at
 * the new root with an empty cache. Every listener is a React prop on an
 * element of this screen, and timers and in-flight loads are dropped on
 * unmount.
 */

import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";

import Breadcrumb from "../components/Breadcrumb";
import FilterBar from "../components/FilterBar";
import GroupingSelector from "../components/GroupingSelector";
import Treemap, {
  type ActivationSource,
  type TreemapHandle,
  type TreemapHit,
} from "../components/Treemap";
import { api, type Grouping, type TreeNodeDto } from "../lib/api";
import { formatBytes, formatCount } from "../lib/format";
import {
  canZoomInto,
  chainTo,
  indexTree,
  joinPath,
  pathOf,
  toTreemapInput,
  type Crumb,
} from "../lib/mapTree";
import { cleared, isFiltered, toFilter, viewKey, type MapView as View } from "../lib/mapView";
import { CATEGORY_LABELS } from "../lib/treemap";
import { hasScan, useAppStore, type ScanSummary } from "../store";

/** Wait this long after a click on a folder, in case it is a double-click. */
const DOUBLE_CLICK_MS = 220;
/** Keeps the tooltip clear of the pointer and the map's edges. */
const TOOLTIP_OFFSET = 14;
/** Wait this long on an item before looking up its path (grouped views). */
const PATH_DELAY_MS = 120;

const IS_MAC = typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);
const FILE_MANAGER = IS_MAC ? "Finder" : "Explorer";

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function MapScreen() {
  const scan = useAppStore((state) => state.scan);
  if (!hasScan(scan)) {
    const text =
      scan.kind === "scanning"
        ? "The map appears when the scan finishes."
        : "The map appears after the first scan. Start one from the Overview.";
    return (
      <section className="empty" aria-labelledby="map-empty">
        <h2 id="map-empty">{text}</h2>
        <p className="empty-text">A zoomable treemap of every folder, sized by space on disk.</p>
      </section>
    );
  }
  return <MapView key={scan.summary.scanId} summary={scan.summary} />;
}

function MapView({ summary }: { readonly summary: ScanSummary }) {
  const { scanId } = summary;
  const [crumbs, setCrumbs] = useState<readonly Crumb[]>([]);
  const [tree, setTree] = useState<TreeNodeDto | null>(null);
  const [enterFrom, setEnterFrom] = useState<number | null>(null);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [hovered, setHovered] = useState<TreemapHit | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const view = useAppStore((state) => state.mapView);
  const setMapView = useAppStore((state) => state.setMapView);
  const key = viewKey(view);
  /** The plain folder view, read with `get_tree`. */
  const plain = view.by === "folder" && !isFiltered(view);
  const grouped = view.by !== "folder";
  const [loading, setLoading] = useState(false);
  /** Real paths of entries shown in a grouped view, looked up on demand. */
  const [paths, setPaths] = useState<ReadonlyMap<number, string>>(new Map());

  const treemapRef = useRef<TreemapHandle | null>(null);
  const tooltipRef = useRef<HTMLDivElement | null>(null);
  const stageRef = useRef<HTMLDivElement | null>(null);
  /** Loaded trees by view key and id; `root` for a view's root. */
  const cache = useRef(new Map<string, TreeNodeDto>());
  /** Bumped by every navigation; a load that finishes late is dropped. */
  const navigation = useRef(0);
  const clickTimer = useRef<number | null>(null);
  const mounted = useRef(true);

  const index = useMemo(() => (tree === null ? null : indexTree(tree)), [tree]);
  const input = useMemo(() => (tree === null ? null : toTreemapInput(tree)), [tree]);

  // `view` is read through `key`: equal views share one cache and one loader.
  const viewRef = useRef<View>(view);
  viewRef.current = view;
  const shown = useRef(false);
  shown.current = tree !== null;
  /** Load `id` in the current view, or the view's root for `null`. */
  const load = useCallback(
    async (id: number | null): Promise<TreeNodeDto> => {
      const cacheKey = `${key}|${id ?? "root"}`;
      const cached = cache.current.get(cacheKey);
      if (cached !== undefined) {
        return cached;
      }
      const current = viewRef.current;
      let loaded: TreeNodeDto;
      if (current.by === "folder" && !isFiltered(current)) {
        loaded = await api.getTree(scanId, id ?? (await api.getOverview(scanId)).rootId);
      } else {
        loaded = await api.getGrouping(scanId, current.by, toFilter(current), id ?? undefined);
      }
      cache.current.set(cacheKey, loaded);
      return loaded;
    },
    [scanId, key],
  );

  const cancelClick = () => {
    if (clickTimer.current !== null) {
      window.clearTimeout(clickTimer.current);
      clickTimer.current = null;
    }
  };

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      navigation.current += 1;
      cancelClick();
    };
  }, []);

  // Open the view's root whenever the view changes (unless a folder is being
  // opened instead: that effect runs after this one and takes over the
  // navigation ticket). The old view stays up until the new one arrives.
  useEffect(() => {
    const ticket = ++navigation.current;
    cancelClick();
    setLoading(true);
    load(null)
      .then((root) => {
        if (mounted.current && ticket === navigation.current) {
          show(root, [{ id: root.id, name: root.name }], null, null);
          setLoading(false);
        }
      })
      .catch((cause: unknown) => {
        if (mounted.current && ticket === navigation.current) {
          if (shown.current) {
            setNotice(message(cause));
          } else {
            setError(message(cause));
          }
          setLoading(false);
        }
      });
    // `show` and `cancelClick` only set state and clear a timer.
  }, [load]);

  /**
   * Show `next` as the map's folder; `trail` is the breadcrumb that leads to
   * it. `from` is the folder just left (zoomed out of), `select` what to
   * select (defaults to `from`).
   */
  const show = (
    next: TreeNodeDto,
    trail: readonly Crumb[],
    from: number | null,
    select: number | null = from,
  ) => {
    setCrumbs(trail);
    setTree(next);
    setEnterFrom(from);
    setSelectedId(select);
    setHovered(null);
    setError(null);
    setLoading(false);
  };

  // "Show in Map" from search: open the entry's folder (or the folder itself)
  // with the entry selected. The request is cleared only once applied, so a
  // remount (StrictMode, or navigating away and back mid-load) retries it.
  const focus = useAppStore((state) => state.mapFocus);
  const mapFocusHandled = useAppStore((state) => state.mapFocusHandled);
  useEffect(() => {
    if (focus === null) {
      return;
    }
    const ticket = ++navigation.current;
    cancelClick();
    api
      .getAncestors(scanId, focus.entryId)
      .then(async (chain) => {
        const trail = focus.isDirectory ? chain : chain.slice(0, -1);
        const folder = trail[trail.length - 1];
        if (folder === undefined) {
          return;
        }
        const next = await load(folder.id);
        if (mounted.current && ticket === navigation.current) {
          show(next, trail, null, focus.isDirectory ? null : focus.entryId);
          setLoading(false);
          mapFocusHandled(focus.nonce);
        }
      })
      .catch((cause: unknown) => {
        if (mounted.current && ticket === navigation.current) {
          setNotice(message(cause));
          setLoading(false);
          mapFocusHandled(focus.nonce);
        }
      });
    // `show` and `cancelClick` only set state and clear a timer, so they are
    // left out of the dependencies.
  }, [focus, scanId, load, mapFocusHandled]);

  const zoomInto = async (id: number) => {
    if (index === null) {
      return;
    }
    const chain = chainTo(index, id);
    if (chain.length === 0) {
      return;
    }
    const ticket = ++navigation.current;
    try {
      const [next] = await Promise.all([
        load(id),
        treemapRef.current?.zoomTo(id) ?? Promise.resolve(),
      ]);
      if (mounted.current && ticket === navigation.current) {
        show(next, [...crumbs, ...chain], null);
      }
    } catch (cause: unknown) {
      if (mounted.current && ticket === navigation.current) {
        setNotice(message(cause));
      }
    }
  };

  const goTo = async (position: number) => {
    const target = crumbs[position];
    // While a new view loads, the map still shows the old one: its ids mean
    // nothing in the new view.
    if (loading || target === undefined || position === crumbs.length - 1) {
      return;
    }
    cancelClick();
    const ticket = ++navigation.current;
    try {
      const next = await load(target.id);
      if (mounted.current && ticket === navigation.current) {
        // Zoom out of the folder we were in, so it is clear where it sits.
        show(next, crumbs.slice(0, position + 1), crumbs[position + 1]?.id ?? null);
      }
    } catch (cause: unknown) {
      if (mounted.current && ticket === navigation.current) {
        setNotice(message(cause));
      }
    }
  };

  const goUp = () => {
    void goTo(crumbs.length - 2);
  };

  const run = (action: "reveal" | "preview", id: number) => {
    if (id < 0) {
      setNotice("That is a group of files, not one file or folder. Open it to see what is inside.");
      return;
    }
    setNotice(null);
    const call = action === "reveal" ? api.revealEntry : api.previewEntry;
    call(scanId, id).catch((cause: unknown) => {
      if (mounted.current) {
        setNotice(message(cause));
      }
    });
  };

  const activate = (hit: TreemapHit, source: ActivationSource) => {
    const node = index?.get(hit.id)?.node;
    if (node === undefined || loading) {
      return;
    }
    cancelClick();
    if (!canZoomInto(node)) {
      setSelectedId(node.id);
      return;
    }
    if (source === "keyboard") {
      void zoomInto(node.id);
      return;
    }
    // A folder click zooms only once it is clearly not a double-click.
    setSelectedId(node.id);
    clickTimer.current = window.setTimeout(() => {
      clickTimer.current = null;
      void zoomInto(node.id);
    }, DOUBLE_CLICK_MS);
  };

  const reveal = (hit: TreemapHit) => {
    if (hit.id < 0) {
      return;
    }
    cancelClick();
    setSelectedId(hit.id);
    run("reveal", hit.id);
  };

  const chooseGrouping = (by: Grouping) => {
    if (by === view.by) {
      return;
    }
    const here = crumbs[crumbs.length - 1];
    if (view.by === "folder") {
      // Leaving the Folder view inside a folder: group that folder.
      const within = crumbs.length > 1 && here !== undefined ? here : null;
      setMapView({ ...view, by, within });
    } else if (by === "folder" && view.within !== null) {
      // Back to Folder: open the folder the grouped view was limited to.
      setMapView({ ...view, by, within: null }, view.within.id);
    } else {
      setMapView({ ...view, by });
    }
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    // The toolbar's own controls keep their keys (Space toggles, Esc closes).
    const target = event.target;
    if (
      target instanceof HTMLInputElement ||
      target instanceof HTMLSelectElement ||
      (target instanceof HTMLElement && target.closest("details") !== null)
    ) {
      return;
    }
    const upChord = event.key === "ArrowUp" && (IS_MAC ? event.metaKey : event.altKey);
    if (event.key === "Escape" || upChord) {
      if (crumbs.length > 1) {
        event.preventDefault();
        goUp();
      }
      return;
    }
    // Space previews what the details line shows. Buttons keep their own Space.
    if (event.key === " " && !(target instanceof HTMLButtonElement)) {
      const shown = hovered?.id ?? selectedId;
      if (shown !== null) {
        event.preventDefault();
        run("preview", shown);
      }
    }
  };

  // The tooltip follows the pointer without re-rendering: position by style.
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const tooltip = tooltipRef.current;
    const stage = stageRef.current;
    if (tooltip === null || stage === null) {
      return;
    }
    const box = stage.getBoundingClientRect();
    let x = event.clientX - box.left + TOOLTIP_OFFSET;
    let y = event.clientY - box.top + TOOLTIP_OFFSET;
    if (x + tooltip.offsetWidth > box.width) {
      x = Math.max(0, x - tooltip.offsetWidth - 2 * TOOLTIP_OFFSET);
    }
    if (y + tooltip.offsetHeight > box.height) {
      y = Math.max(0, y - tooltip.offsetHeight - 2 * TOOLTIP_OFFSET);
    }
    tooltip.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
  };

  // A grouped view's entries come from all over the scan: look up the real
  // path of what the details line shows, once the pointer rests on it.
  const detailId = hovered?.id ?? selectedId ?? tree?.id ?? null;
  useEffect(() => {
    if (!grouped || detailId === null || detailId < 0 || paths.has(detailId)) {
      return;
    }
    const timer = window.setTimeout(() => {
      api
        .getAncestors(scanId, detailId)
        .then((chain) => {
          if (mounted.current) {
            const names = chain.slice(1).map((crumb) => crumb.name);
            setPaths((known) => new Map(known).set(detailId, joinPath(summary.root, names)));
          }
        })
        .catch(() => {
          // The details line just shows no path.
        });
    }, PATH_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [grouped, detailId, paths, scanId, summary.root]);

  if (error !== null) {
    return (
      <section className="empty" role="alert">
        <h2>The map could not be loaded</h2>
        <p className="empty-text">{error}</p>
      </section>
    );
  }

  const current = crumbs[crumbs.length - 1];
  const describe = (id: number) => {
    const node = index?.get(id)?.node;
    if (node === undefined || index === null) {
      return null;
    }
    // A grouped view's breadcrumb is groups, not folders: a real entry's path
    // is looked up (below), a group shows where it sits in the view.
    const trail = [...crumbs.map((crumb) => crumb.name), ...chainTo(index, id).map((c) => c.name)];
    const path = !grouped
      ? pathOf(summary.root, crumbs, index, id)
      : id >= 0
        ? (paths.get(id) ?? "")
        : trail.join(" › ");
    return {
      node,
      path,
      category: id < 0 ? "Group" : node.kind === "Dir" ? "Folder" : CATEGORY_LABELS[node.category],
    };
  };
  const hoverInfo = hovered === null ? null : describe(hovered.id);
  const detail =
    hoverInfo ?? (selectedId === null ? null : describe(selectedId)) ?? (tree === null ? null : describe(tree.id));

  // Only a filtered view can be empty; an empty folder is just an empty map.
  const empty = tree !== null && !plain && isFiltered(view) && crumbs.length === 1 && tree.sizeAlloc === 0;

  const label =
    tree === null
      ? "Storage map, loading"
      : `Storage map of ${tree.name}: ${formatBytes(tree.sizeAlloc)} in ${formatCount(
          tree.fileCount,
        )} files. Arrow keys select, Enter opens a folder, Escape goes up.`;

  return (
    <div className="map-screen" onKeyDown={onKeyDown}>
      <div className="map-controls">
        <GroupingSelector value={view.by} onChange={chooseGrouping} />
        <FilterBar view={view} onChange={(next) => setMapView(next)} />
      </div>
      <div className="map-toolbar">
        {current === undefined ? <span className="map-loading">Loading…</span> : null}
        <Breadcrumb crumbs={crumbs} onNavigate={(position) => void goTo(position)} />
        {loading && current !== undefined ? (
          <span className="map-loading" aria-live="polite">
            Updating…
          </span>
        ) : null}
      </div>

      <div ref={stageRef} className="map-stage" onPointerMove={onPointerMove}>
        <Treemap
          ref={treemapRef}
          root={input}
          selectedId={selectedId}
          enterFrom={enterFrom}
          onActivate={activate}
          onReveal={reveal}
          onSelectionChange={(hit) => setSelectedId(hit.id)}
          onHover={setHovered}
          label={label}
        />
        {empty ? (
          <div className="map-empty">
            <p>Nothing in this scan matches these filters.</p>
            <button type="button" className="button" onClick={() => setMapView(cleared(view))}>
              Clear filters
            </button>
          </div>
        ) : null}
        <div
          ref={tooltipRef}
          className="map-tooltip"
          role="tooltip"
          hidden={hoverInfo === null}
        >
          {hoverInfo === null ? null : (
            <>
              <strong className="map-tooltip-name">{hoverInfo.node.name}</strong>
              <span>
                {formatBytes(hoverInfo.node.sizeAlloc)} · {hoverInfo.category}
                {hoverInfo.node.kind === "Dir"
                  ? ` · ${formatCount(hoverInfo.node.fileCount)} files`
                  : ""}
              </span>
              <span className="map-tooltip-path">{hoverInfo.path}</span>
            </>
          )}
        </div>
      </div>

      <div className="map-details" aria-live="polite">
        {detail === null ? (
          <span>Loading…</span>
        ) : (
          <>
            <span className="map-details-name">{detail.node.name}</span>
            <span>{formatBytes(detail.node.sizeAlloc)}</span>
            <span>{detail.category}</span>
            <span className="map-details-path" title={detail.path}>
              {detail.path}
            </span>
          </>
        )}
      </div>
      <p className="map-hints">
        {notice !== null ? (
          <span className="map-notice" role="alert">
            {notice}
          </span>
        ) : (
          <>
            Click {grouped ? "a group" : "a folder"} to zoom · Double-click to show in {FILE_MANAGER} ·{" "}
            {IS_MAC ? "Space for Quick Look" : "Space to show in Explorer"} · Esc or{" "}
            {IS_MAC ? "⌘↑" : "Alt+↑"} to go up
          </>
        )}
      </p>
    </div>
  );
}

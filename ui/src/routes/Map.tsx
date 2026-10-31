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
 * State is per scan: the screen is keyed by scan id, so a rescan starts over at
 * the new root with an empty cache. Every listener is a React prop on an
 * element of this screen, and timers and in-flight loads are dropped on
 * unmount.
 */

import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";

import Breadcrumb from "../components/Breadcrumb";
import Treemap, {
  type ActivationSource,
  type TreemapHandle,
  type TreemapHit,
} from "../components/Treemap";
import { api, type TreeNodeDto } from "../lib/api";
import { formatBytes, formatCount } from "../lib/format";
import {
  canZoomInto,
  chainTo,
  indexTree,
  pathOf,
  toTreemapInput,
  type Crumb,
} from "../lib/mapTree";
import { CATEGORY_LABELS } from "../lib/treemap";
import { hasScan, useAppStore, type ScanSummary } from "../store";

/** Wait this long after a click on a folder, in case it is a double-click. */
const DOUBLE_CLICK_MS = 220;
/** Keeps the tooltip clear of the pointer and the map's edges. */
const TOOLTIP_OFFSET = 14;

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

  const treemapRef = useRef<TreemapHandle | null>(null);
  const tooltipRef = useRef<HTMLDivElement | null>(null);
  const stageRef = useRef<HTMLDivElement | null>(null);
  const cache = useRef(new Map<number, TreeNodeDto>());
  /** Bumped by every navigation; a load that finishes late is dropped. */
  const navigation = useRef(0);
  const clickTimer = useRef<number | null>(null);
  const mounted = useRef(true);

  const index = useMemo(() => (tree === null ? null : indexTree(tree)), [tree]);
  const input = useMemo(() => (tree === null ? null : toTreemapInput(tree)), [tree]);

  const load = useCallback(
    async (id: number): Promise<TreeNodeDto> => {
      const cached = cache.current.get(id);
      if (cached !== undefined) {
        return cached;
      }
      const loaded = await api.getTree(scanId, id);
      cache.current.set(id, loaded);
      return loaded;
    },
    [scanId],
  );

  const cancelClick = () => {
    if (clickTimer.current !== null) {
      window.clearTimeout(clickTimer.current);
      clickTimer.current = null;
    }
  };

  // Open on the scan root.
  useEffect(() => {
    mounted.current = true;
    const ticket = ++navigation.current;
    api
      .getOverview(scanId)
      .then(async (overview) => {
        const root = await load(overview.rootId);
        if (mounted.current && ticket === navigation.current) {
          setCrumbs([{ id: root.id, name: root.name }]);
          setTree(root);
        }
      })
      .catch((cause: unknown) => {
        if (mounted.current) {
          setError(message(cause));
        }
      });
    return () => {
      mounted.current = false;
      navigation.current += 1;
      cancelClick();
    };
  }, [scanId, load]);

  /** Show `next` as the map's folder; `trail` is the breadcrumb that leads to it. */
  const show = (next: TreeNodeDto, trail: readonly Crumb[], from: number | null) => {
    setCrumbs(trail);
    setTree(next);
    setEnterFrom(from);
    setSelectedId(from);
    setHovered(null);
    setError(null);
  };

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
    if (target === undefined || position === crumbs.length - 1) {
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
    if (node === undefined) {
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
    cancelClick();
    setSelectedId(hit.id);
    run("reveal", hit.id);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const upChord = event.key === "ArrowUp" && (IS_MAC ? event.metaKey : event.altKey);
    if (event.key === "Escape" || upChord) {
      if (crumbs.length > 1) {
        event.preventDefault();
        goUp();
      }
      return;
    }
    // Space previews what the details line shows. Buttons keep their own Space.
    if (event.key === " " && !(event.target instanceof HTMLButtonElement)) {
      const target = hovered?.id ?? selectedId;
      if (target !== null) {
        event.preventDefault();
        run("preview", target);
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
    return {
      node,
      path: pathOf(summary.root, crumbs, index, id),
      category: node.kind === "Dir" ? "Folder" : CATEGORY_LABELS[node.category],
    };
  };
  const hoverInfo = hovered === null ? null : describe(hovered.id);
  const detail =
    hoverInfo ?? (selectedId === null ? null : describe(selectedId)) ?? (tree === null ? null : describe(tree.id));

  const label =
    tree === null
      ? "Storage map, loading"
      : `Storage map of ${tree.name}: ${formatBytes(tree.sizeAlloc)} in ${formatCount(
          tree.fileCount,
        )} files. Arrow keys select, Enter opens a folder, Escape goes up.`;

  return (
    <div className="map-screen" onKeyDown={onKeyDown}>
      <div className="map-toolbar">
        {current === undefined ? <span className="map-loading">Loading…</span> : null}
        <Breadcrumb crumbs={crumbs} onNavigate={(position) => void goTo(position)} />
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
            Click a folder to zoom · Double-click to show in {FILE_MANAGER} ·{" "}
            {IS_MAC ? "Space for Quick Look" : "Space to show in Explorer"} · Esc or{" "}
            {IS_MAC ? "⌘↑" : "Alt+↑"} to go up
          </>
        )}
      </p>
    </div>
  );
}

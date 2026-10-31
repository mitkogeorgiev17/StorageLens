/**
 * Canvas treemap (PLAN §13). Not SVG: 1,500 rects in the DOM crawl.
 *
 * - The layout is cached (`useMemo` on data and size) and never recomputed per
 *   frame; pan and zoom only change the {@link View} applied at draw time.
 * - Frames are drawn on `requestAnimationFrame`, at most one per display frame,
 *   and only when something changed.
 * - The backing store follows `devicePixelRatio`, so the map is sharp at 2×.
 * - Wheel zooms around the cursor, drag pans, click selects. Click-to-zoom,
 *   breadcrumbs, and the tooltip belong to the Map screen (#21), which drives
 *   this component through `onSelect` / `onHover`.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { formatBytes, formatCount } from "../lib/format";
import {
  CATEGORY_ORDER,
  IDENTITY_VIEW,
  backingStore,
  clampView,
  colorOf,
  computeFrame,
  fitLabel,
  hitTest,
  layoutTreemap,
  screenRect,
  zoomAt,
  type Frame,
  type Layout,
  type TreemapInput,
  type View,
} from "../lib/treemap";
import type { Category } from "../lib/api";

/** What the pointer is over. */
export interface TreemapHit {
  readonly id: number;
  readonly name: string;
  readonly sizeAlloc: number;
  readonly category: Category;
  readonly isDirectory: boolean;
}

export interface TreemapProps {
  readonly root: TreemapInput | null;
  readonly onSelect?: (hit: TreemapHit) => void;
  readonly onHover?: (hit: TreemapHit | null) => void;
  /** Accessible summary of what the map shows. */
  readonly label: string;
}

const LABEL_FONT_SIZE = 11;
const WHEEL_ZOOM_PER_PIXEL = 0.0015;
/** Pointer travel, px, that turns a click into a drag. */
const DRAG_THRESHOLD = 4;

interface Theme {
  readonly background: string;
  readonly directory: string;
  readonly directoryText: string;
  readonly leafText: string;
  readonly highlight: string;
  readonly tile: string;
  readonly tileText: string;
  readonly font: string;
}

function readTheme(element: Element): Theme {
  const style = getComputedStyle(element);
  const token = (name: string, fallback: string) => style.getPropertyValue(name).trim() || fallback;
  return {
    background: token("--treemap-gap", "#e5e5ea"),
    directory: token("--treemap-dir", "#d4d4dc"),
    directoryText: token("--text", "#1c1c21"),
    leafText: token("--treemap-leaf-text", "#111114"),
    highlight: token("--accent", "#2459d6"),
    tile: token("--surface", "#ffffff"),
    tileText: token("--text", "#1c1c21"),
    font: token("--font-sans", "system-ui, sans-serif"),
  };
}

function hitOf(layout: Layout, index: number): TreemapHit {
  return {
    id: layout.ids[index] ?? 0,
    name: layout.names[index] ?? "",
    sizeAlloc: layout.value[index] ?? 0,
    category: CATEGORY_ORDER[layout.category[index] ?? CATEGORY_ORDER.length - 1] ?? "other",
    isDirectory: layout.leaf[index] === 0,
  };
}

function draw(
  context: CanvasRenderingContext2D,
  layout: Layout,
  frame: Frame,
  view: View,
  width: number,
  height: number,
  theme: Theme,
  hovered: number | null,
): void {
  context.fillStyle = theme.background;
  context.fillRect(0, 0, width, height);
  context.font = `${LABEL_FONT_SIZE}px ${theme.font}`;
  context.textBaseline = "middle";

  for (const index of frame.drawn) {
    const rect = screenRect(layout, index, view);
    const isLeaf = layout.leaf[index] === 1;
    context.fillStyle = isLeaf ? colorOf(layout, index) : theme.directory;
    context.fillRect(rect.x, rect.y, rect.width, rect.height);

    // Directories label their header strip; leaves label their centre line.
    if (rect.height >= LABEL_FONT_SIZE + 4) {
      const text = fitLabel(layout.names[index] ?? "", rect.width, LABEL_FONT_SIZE);
      if (text !== null) {
        context.fillStyle = isLeaf ? theme.leafText : theme.directoryText;
        const y = isLeaf ? rect.y + Math.min(rect.height / 2, 10) : rect.y + 8;
        context.fillText(text, rect.x + 3, y, rect.width - 6);
      }
    }
  }

  if (hovered !== null) {
    const rect = screenRect(layout, hovered, view);
    context.strokeStyle = theme.highlight;
    context.lineWidth = 2;
    context.strokeRect(rect.x + 1, rect.y + 1, Math.max(0, rect.width - 2), Math.max(0, rect.height - 2));
  }

  if (frame.hiddenCount > 0) {
    const text = `+${formatCount(frame.hiddenCount)} more · ${formatBytes(frame.hiddenBytes)}`;
    const tileWidth = Math.min(width - 16, text.length * LABEL_FONT_SIZE * 0.6 + 16);
    const tileHeight = 22;
    const x = width - tileWidth - 8;
    const y = height - tileHeight - 8;
    context.globalAlpha = 0.92;
    context.fillStyle = theme.tile;
    context.fillRect(x, y, tileWidth, tileHeight);
    context.globalAlpha = 1;
    context.fillStyle = theme.tileText;
    context.fillText(text, x + 8, y + tileHeight / 2, tileWidth - 16);
  }
}

export default function Treemap({ root, onSelect, onHover, label }: TreemapProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [size, setSize] = useState({ width: 0, height: 0, dpr: 1 });

  const viewRef = useRef<View>(IDENTITY_VIEW);
  const frameRef = useRef<Frame | null>(null);
  const hoveredRef = useRef<number | null>(null);
  const pendingRef = useRef<number | null>(null);
  const dragRef = useRef<{ x: number; y: number; view: View; moved: boolean } | null>(null);

  // Track the container's CSS size and the display's pixel ratio. A window
  // moving to another monitor fires `resize`, which re-reads the ratio.
  useEffect(() => {
    const container = containerRef.current;
    if (container === null) {
      return;
    }
    const measure = () => {
      const box = container.getBoundingClientRect();
      setSize((previous) => {
        const next = {
          width: Math.floor(box.width),
          height: Math.floor(box.height),
          dpr: window.devicePixelRatio,
        };
        return previous.width === next.width &&
          previous.height === next.height &&
          previous.dpr === next.dpr
          ? previous
          : next;
      });
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(container);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, []);

  // The expensive step: only on new data or a new size, never per frame.
  const layout = useMemo(
    () =>
      root === null || size.width === 0 || size.height === 0
        ? null
        : layoutTreemap(root, size.width, size.height),
    [root, size.width, size.height],
  );

  const render = useCallback(() => {
    pendingRef.current = null;
    const canvas = canvasRef.current;
    const context = canvas?.getContext("2d");
    if (canvas === null || canvas === undefined || context === null || context === undefined) {
      return;
    }
    const store = backingStore(size.width, size.height, size.dpr);
    if (canvas.width !== store.width || canvas.height !== store.height) {
      canvas.width = store.width;
      canvas.height = store.height;
    }
    context.setTransform(store.scale, 0, 0, store.scale, 0, 0);

    const theme = readTheme(canvas);
    if (layout === null) {
      context.fillStyle = theme.background;
      context.fillRect(0, 0, size.width, size.height);
      frameRef.current = null;
      return;
    }
    const view = viewRef.current;
    const frame = computeFrame(layout, view, size.width, size.height);
    frameRef.current = frame;
    draw(context, layout, frame, view, size.width, size.height, theme, hoveredRef.current);
  }, [layout, size]);

  const schedule = useCallback(() => {
    if (pendingRef.current === null) {
      pendingRef.current = requestAnimationFrame(render);
    }
  }, [render]);

  // New layout: start from the whole map.
  useEffect(() => {
    viewRef.current = IDENTITY_VIEW;
    hoveredRef.current = null;
    schedule();
  }, [layout, schedule]);

  useEffect(
    () => () => {
      if (pendingRef.current !== null) {
        cancelAnimationFrame(pendingRef.current);
      }
    },
    [],
  );

  // Redraw when the OS theme flips: colours come from CSS tokens.
  useEffect(() => {
    const query = window.matchMedia("(prefers-color-scheme: dark)");
    query.addEventListener("change", schedule);
    return () => query.removeEventListener("change", schedule);
  }, [schedule]);

  // Wheel zoom needs a non-passive listener to stop the page from scrolling.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (canvas === null) {
      return;
    }
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      const box = canvas.getBoundingClientRect();
      const factor = Math.exp(-event.deltaY * WHEEL_ZOOM_PER_PIXEL);
      viewRef.current = clampView(
        zoomAt(viewRef.current, factor, event.clientX - box.left, event.clientY - box.top),
        size.width,
        size.height,
      );
      schedule();
    };
    canvas.addEventListener("wheel", onWheel, { passive: false });
    return () => canvas.removeEventListener("wheel", onWheel);
  }, [schedule, size.width, size.height]);

  const pointAt = (event: React.PointerEvent<HTMLCanvasElement>) => {
    const box = event.currentTarget.getBoundingClientRect();
    return { x: event.clientX - box.left, y: event.clientY - box.top };
  };

  const hitAt = (x: number, y: number): number | null => {
    const frame = frameRef.current;
    return layout === null || frame === null ? null : hitTest(layout, frame, viewRef.current, x, y);
  };

  const onPointerDown = (event: React.PointerEvent<HTMLCanvasElement>) => {
    const point = pointAt(event);
    dragRef.current = { ...point, view: viewRef.current, moved: false };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const onPointerMove = (event: React.PointerEvent<HTMLCanvasElement>) => {
    const point = pointAt(event);
    const drag = dragRef.current;
    if (drag !== null) {
      const dx = point.x - drag.x;
      const dy = point.y - drag.y;
      if (drag.moved || Math.hypot(dx, dy) > DRAG_THRESHOLD) {
        drag.moved = true;
        viewRef.current = clampView(
          {
            scale: drag.view.scale,
            translateX: drag.view.translateX + dx,
            translateY: drag.view.translateY + dy,
          },
          size.width,
          size.height,
        );
        schedule();
        return;
      }
    }

    const hovered = hitAt(point.x, point.y);
    if (hovered !== hoveredRef.current) {
      hoveredRef.current = hovered;
      schedule();
      if (onHover !== undefined) {
        onHover(hovered === null || layout === null ? null : hitOf(layout, hovered));
      }
    }
  };

  const onPointerUp = (event: React.PointerEvent<HTMLCanvasElement>) => {
    const drag = dragRef.current;
    dragRef.current = null;
    if (drag === null || drag.moved || layout === null || onSelect === undefined) {
      return;
    }
    const point = pointAt(event);
    const hit = hitAt(point.x, point.y);
    if (hit !== null) {
      onSelect(hitOf(layout, hit));
    }
  };

  const onPointerLeave = () => {
    if (hoveredRef.current !== null) {
      hoveredRef.current = null;
      schedule();
      onHover?.(null);
    }
  };

  return (
    <div ref={containerRef} className="treemap">
      <canvas
        ref={canvasRef}
        className="treemap-canvas"
        role="img"
        aria-label={label}
        style={{ width: size.width, height: size.height }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerLeave={onPointerLeave}
      />
    </div>
  );
}

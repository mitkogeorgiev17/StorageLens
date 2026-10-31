/**
 * Canvas treemap (PLAN §13). Not SVG: 1,500 rects in the DOM crawl.
 *
 * - The layout is cached (`useMemo` on data and size) and never recomputed per
 *   frame; pan and zoom only change the {@link View} applied at draw time.
 * - Frames are drawn on `requestAnimationFrame`, at most one per display frame,
 *   and only when something changed.
 * - The backing store follows `devicePixelRatio`, so the map is sharp at 2×.
 * - Wheel zooms around the cursor, drag pans. Clicks, double-clicks, and keys
 *   are reported, not acted on: the Map screen decides what they mean.
 * - Keyboard: arrows move the selection between top-level rects, Enter
 *   activates it, ⌘/Ctrl+Enter reveals it — the same actions as click and
 *   double-click.
 * - Transitions: {@link TreemapHandle.zoomTo} animates into a rect before the
 *   screen swaps in that folder's own layout; `enterFrom` animates out of the
 *   folder just left. Both are skipped under `prefers-reduced-motion`.
 */

import {
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
  type Ref,
} from "react";

import { formatBytes, formatCount } from "../lib/format";
import {
  CATEGORY_ORDER,
  IDENTITY_VIEW,
  backingStore,
  clampView,
  colorOf,
  computeFrame,
  easeInOut,
  fitLabel,
  fitView,
  hitTest,
  indexOfId,
  interpolateView,
  layoutTreemap,
  neighbor,
  screenRect,
  zoomAt,
  type Direction,
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

/** How an activation happened: pointer clicks may still become double-clicks. */
export type ActivationSource = "pointer" | "keyboard";

export interface TreemapHandle {
  /** Animate the view into rect `id`; resolves when done (at once if absent). */
  zoomTo(id: number): Promise<void>;
}

export interface TreemapProps {
  readonly root: TreemapInput | null;
  /** Outlined, and the starting point for arrow keys. */
  readonly selectedId?: number | null;
  /** Click (pointer) or Enter (keyboard). */
  readonly onActivate?: (hit: TreemapHit, source: ActivationSource) => void;
  /** Double-click or ⌘/Ctrl+Enter. */
  readonly onReveal?: (hit: TreemapHit) => void;
  /** Arrow keys moved the selection. */
  readonly onSelectionChange?: (hit: TreemapHit) => void;
  readonly onHover?: (hit: TreemapHit | null) => void;
  /** On a new layout, animate out of this rect (the folder just left). */
  readonly enterFrom?: number | null;
  /** Accessible summary of what the map shows. */
  readonly label: string;
  readonly ref?: Ref<TreemapHandle>;
}

const LABEL_FONT_SIZE = 11;
const WHEEL_ZOOM_PER_PIXEL = 0.0015;
/** Pointer travel, px, that turns a click into a drag. */
const DRAG_THRESHOLD = 4;
const TRANSITION_MS = 320;

const ARROWS: Readonly<Record<string, Direction>> = {
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
};

function reducedMotion(): boolean {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

interface Transition {
  readonly from: View;
  readonly to: View;
  readonly start: number;
  readonly duration: number;
  readonly done: () => void;
}

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
  selected: number | null,
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

  const outline = (index: number, color: string, lineWidth: number) => {
    const rect = screenRect(layout, index, view);
    const inset = lineWidth / 2;
    context.strokeStyle = color;
    context.lineWidth = lineWidth;
    context.strokeRect(
      rect.x + inset,
      rect.y + inset,
      Math.max(0, rect.width - lineWidth),
      Math.max(0, rect.height - lineWidth),
    );
  };
  if (selected !== null) {
    outline(selected, theme.highlight, 3);
  }
  if (hovered !== null && hovered !== selected) {
    outline(hovered, theme.directoryText, 1.5);
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

export default function Treemap({
  root,
  selectedId = null,
  onActivate,
  onReveal,
  onSelectionChange,
  onHover,
  enterFrom = null,
  label,
  ref,
}: TreemapProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [size, setSize] = useState({ width: 0, height: 0, dpr: 1 });

  const viewRef = useRef<View>(IDENTITY_VIEW);
  const frameRef = useRef<Frame | null>(null);
  const hoveredRef = useRef<number | null>(null);
  const pendingRef = useRef<number | null>(null);
  const transitionRef = useRef<Transition | null>(null);
  const dragRef = useRef<{ x: number; y: number; view: View; moved: boolean } | null>(null);
  /** Whether the last pointer-down turned into a drag; suppresses its click. */
  const draggedRef = useRef(false);

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

  const selected = layout === null || selectedId === null ? null : indexOfId(layout, selectedId);
  const selectedRef = useRef<number | null>(selected);
  selectedRef.current = selected;

  const scheduleRef = useRef<() => void>(() => undefined);

  const render = useCallback(
    (now: number) => {
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

      const transition = transitionRef.current;
      if (transition !== null) {
        const t = transition.duration <= 0 ? 1 : (now - transition.start) / transition.duration;
        viewRef.current = interpolateView(transition.from, transition.to, easeInOut(t), size.width);
        if (t >= 1) {
          transitionRef.current = null;
          transition.done();
        } else {
          scheduleRef.current();
        }
      }

      const view = viewRef.current;
      const frame = computeFrame(layout, view, size.width, size.height);
      frameRef.current = frame;
      draw(
        context,
        layout,
        frame,
        view,
        size.width,
        size.height,
        theme,
        hoveredRef.current,
        selectedRef.current,
      );
    },
    [layout, size],
  );

  const schedule = useCallback(() => {
    if (pendingRef.current === null) {
      pendingRef.current = requestAnimationFrame(render);
    }
  }, [render]);
  scheduleRef.current = schedule;

  /** End any running transition where it is, settling its promise. */
  const stopTransition = useCallback(() => {
    const transition = transitionRef.current;
    transitionRef.current = null;
    transition?.done();
  }, []);

  const animate = useCallback(
    (from: View, to: View): Promise<void> => {
      stopTransition();
      return new Promise((resolve) => {
        transitionRef.current = {
          from,
          to,
          start: performance.now(),
          duration: reducedMotion() ? 0 : TRANSITION_MS,
          done: resolve,
        };
        schedule();
      });
    },
    [schedule, stopTransition],
  );

  useImperativeHandle(
    ref,
    () => ({
      zoomTo: (id: number) => {
        const index = layout === null ? null : indexOfId(layout, id);
        if (layout === null || index === null) {
          return Promise.resolve();
        }
        return animate(viewRef.current, fitView(layout, index, size.width, size.height));
      },
    }),
    [animate, layout, size.width, size.height],
  );

  // New layout: start from the whole map — or, going up, from the folder just
  // left, zooming out so it is clear where that folder sits.
  const enterFromRef = useRef(enterFrom);
  enterFromRef.current = enterFrom;
  useEffect(() => {
    stopTransition();
    hoveredRef.current = null;
    viewRef.current = IDENTITY_VIEW;
    const from = enterFromRef.current;
    const index = layout === null || from === null ? null : indexOfId(layout, from);
    if (layout !== null && index !== null && !reducedMotion()) {
      viewRef.current = fitView(layout, index, size.width, size.height);
      void animate(viewRef.current, IDENTITY_VIEW);
    }
    schedule();
  }, [layout, schedule, animate, stopTransition, size.width, size.height]);

  // Selection changes redraw the outline.
  useEffect(() => {
    schedule();
  }, [selected, schedule]);

  // Unmount: no frame or transition outlives the component.
  useEffect(
    () => () => {
      if (pendingRef.current !== null) {
        cancelAnimationFrame(pendingRef.current);
        pendingRef.current = null;
      }
      const transition = transitionRef.current;
      transitionRef.current = null;
      transition?.done();
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
      stopTransition();
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
  }, [schedule, stopTransition, size.width, size.height]);

  const pointAt = (event: PointerEvent<HTMLCanvasElement> | MouseEvent<HTMLCanvasElement>) => {
    const box = event.currentTarget.getBoundingClientRect();
    return { x: event.clientX - box.left, y: event.clientY - box.top };
  };

  const hitAt = (x: number, y: number): number | null => {
    const frame = frameRef.current;
    return layout === null || frame === null ? null : hitTest(layout, frame, viewRef.current, x, y);
  };

  const hitUnder = (event: MouseEvent<HTMLCanvasElement>): TreemapHit | null => {
    const point = pointAt(event);
    const index = hitAt(point.x, point.y);
    return index === null || layout === null ? null : hitOf(layout, index);
  };

  const onPointerDown = (event: PointerEvent<HTMLCanvasElement>) => {
    const point = pointAt(event);
    dragRef.current = { ...point, view: viewRef.current, moved: false };
    draggedRef.current = false;
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const onPointerMove = (event: PointerEvent<HTMLCanvasElement>) => {
    const point = pointAt(event);
    const drag = dragRef.current;
    if (drag !== null) {
      const dx = point.x - drag.x;
      const dy = point.y - drag.y;
      if (drag.moved || Math.hypot(dx, dy) > DRAG_THRESHOLD) {
        if (!drag.moved) {
          stopTransition();
        }
        drag.moved = true;
        draggedRef.current = true;
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
      onHover?.(hovered === null || layout === null ? null : hitOf(layout, hovered));
    }
  };

  const onPointerUp = () => {
    dragRef.current = null;
  };

  const onClick = (event: MouseEvent<HTMLCanvasElement>) => {
    // The second click of a double-click is the double-click's business.
    if (draggedRef.current || event.detail > 1) {
      return;
    }
    const hit = hitUnder(event);
    if (hit !== null) {
      onActivate?.(hit, "pointer");
    }
  };

  const onDoubleClick = (event: MouseEvent<HTMLCanvasElement>) => {
    if (draggedRef.current) {
      return;
    }
    const hit = hitUnder(event);
    if (hit !== null) {
      onReveal?.(hit);
    }
  };

  const onPointerLeave = () => {
    if (hoveredRef.current !== null) {
      hoveredRef.current = null;
      schedule();
      onHover?.(null);
    }
  };

  const onKeyDown = (event: KeyboardEvent<HTMLCanvasElement>) => {
    if (layout === null) {
      return;
    }
    const direction = ARROWS[event.key];
    const modified = event.metaKey || event.ctrlKey || event.altKey;
    if (direction !== undefined && !modified) {
      event.preventDefault();
      const next = neighbor(layout, selectedRef.current, direction);
      if (next !== null && next !== selectedRef.current) {
        onSelectionChange?.(hitOf(layout, next));
      }
      return;
    }
    if (event.key === "Enter" && selectedRef.current !== null) {
      event.preventDefault();
      const hit = hitOf(layout, selectedRef.current);
      if (event.metaKey || event.ctrlKey) {
        onReveal?.(hit);
      } else {
        onActivate?.(hit, "keyboard");
      }
    }
  };

  return (
    <div ref={containerRef} className="treemap">
      <canvas
        ref={canvasRef}
        className="treemap-canvas"
        // Arrow keys belong to the map, so screen readers must pass them through.
        role="application"
        aria-roledescription="storage map"
        tabIndex={0}
        aria-label={label}
        style={{ width: size.width, height: size.height }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onPointerLeave={onPointerLeave}
        onClick={onClick}
        onDoubleClick={onDoubleClick}
        onKeyDown={onKeyDown}
      />
    </div>
  );
}

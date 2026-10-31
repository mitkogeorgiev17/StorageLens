/**
 * Canvas sunburst (#27): the Map's alternate view, with the treemap's props and
 * handle so the Map screen swaps one for the other and keeps its navigation,
 * grouping, and filters.
 *
 * - The partition layout is cached per tree; a frame only maps it through the
 *   current {@link SunView} and draws the arcs `visibleArcs` keeps.
 * - Hover highlights the path from the centre to the arc under the pointer.
 * - Click an arc to zoom (the screen decides, as with the treemap); click the
 *   centre to go up. Double-click / ⌘/Ctrl+Enter reveals.
 * - Keyboard: ←/→ move between siblings, ↓ outward to the biggest child, ↑
 *   inward to the parent; Enter activates.
 * - Transitions animate the angular domain (zoom in) or out of the folder just
 *   left, skipped under `prefers-reduced-motion`.
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

import type { Category } from "../lib/api";
import { formatBytes } from "../lib/format";
import {
  FULL_VIEW,
  ancestorPath,
  arcOf,
  fitSun,
  geometry,
  hitTestSunburst,
  indexOfIdSun,
  interpolateSun,
  layoutSunburst,
  neighborSun,
  spanArc,
  sunColor,
  visibleArcs,
  type Arc,
  type Geometry,
  type SunDirection,
  type SunView,
  type SunburstLayout,
} from "../lib/sunburst";
import { backingStore, CATEGORY_ORDER, easeInOut, fitLabel, type TreemapInput } from "../lib/treemap";
import type { ActivationSource, TreemapHandle, TreemapHit } from "./Treemap";

export interface SunburstProps {
  readonly root: TreemapInput | null;
  readonly selectedId?: number | null;
  readonly onActivate?: (hit: TreemapHit, source: ActivationSource) => void;
  readonly onReveal?: (hit: TreemapHit) => void;
  readonly onSelectionChange?: (hit: TreemapHit) => void;
  readonly onHover?: (hit: TreemapHit | null) => void;
  /** The centre was clicked: go up a level. */
  readonly onBack?: () => void;
  readonly enterFrom?: number | null;
  readonly label: string;
  readonly ref?: Ref<TreemapHandle>;
}

const LABEL_FONT_SIZE = 11;
const TRANSITION_MS = 360;
/** Path highlighting dims everything else to this opacity. */
const DIMMED = 0.35;

const KEYS: Readonly<Record<string, SunDirection>> = {
  ArrowLeft: "previous",
  ArrowRight: "next",
  ArrowUp: "in",
  ArrowDown: "out",
};

function reducedMotion(): boolean {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

interface Transition {
  readonly from: SunView;
  readonly to: SunView;
  readonly start: number;
  readonly duration: number;
  readonly done: () => void;
}

interface Theme {
  readonly background: string;
  readonly directory: string;
  readonly text: string;
  readonly leafText: string;
  readonly muted: string;
  readonly highlight: string;
  readonly font: string;
}

function readTheme(element: Element): Theme {
  const style = getComputedStyle(element);
  const token = (name: string, fallback: string) => style.getPropertyValue(name).trim() || fallback;
  return {
    background: token("--surface", "#ffffff"),
    directory: token("--treemap-dir", "#d4d4dc"),
    text: token("--text", "#1c1c21"),
    leafText: token("--treemap-leaf-text", "#111114"),
    muted: token("--text-muted", "#5b5b66"),
    highlight: token("--accent", "#2459d6"),
    font: token("--font-sans", "system-ui, sans-serif"),
  };
}

function hitOf(layout: SunburstLayout, index: number): TreemapHit {
  return {
    id: layout.ids[index] ?? 0,
    name: layout.names[index] ?? "",
    sizeAlloc: layout.value[index] ?? 0,
    category: (CATEGORY_ORDER[layout.category[index] ?? CATEGORY_ORDER.length - 1] ??
      "other") as Category,
    isDirectory: layout.leaf[index] === 0,
  };
}

/** Trace arc `arc` around `geo`'s centre (0 rad = up, clockwise). */
function traceArc(context: CanvasRenderingContext2D, geo: Geometry, arc: Arc): void {
  const start = arc.start - Math.PI / 2;
  const end = arc.end - Math.PI / 2;
  context.beginPath();
  if (arc.inner <= 0) {
    if (arc.end - arc.start >= 2 * Math.PI - 1e-9) {
      context.arc(geo.cx, geo.cy, arc.outer, 0, 2 * Math.PI);
    } else {
      context.moveTo(geo.cx, geo.cy);
      context.arc(geo.cx, geo.cy, arc.outer, start, end);
    }
  } else {
    context.arc(geo.cx, geo.cy, arc.outer, start, end);
    context.arc(geo.cx, geo.cy, arc.inner, end, start, true);
  }
  context.closePath();
}

function draw(
  context: CanvasRenderingContext2D,
  layout: SunburstLayout,
  drawn: Uint32Array,
  view: SunView,
  geo: Geometry,
  width: number,
  height: number,
  theme: Theme,
  hovered: number | null,
  selected: number | null,
): void {
  context.fillStyle = theme.background;
  context.fillRect(0, 0, width, height);
  const path = hovered === null ? null : ancestorPath(layout, hovered);

  // Bytes the backend did not send: neutral, never mistaken for free space.
  context.globalAlpha = path === null ? 0.5 : 0.5 * DIMMED;
  context.fillStyle = theme.directory;
  for (let offset = 0; offset + 2 < layout.rest.length; offset += 3) {
    const arc = spanArc(
      layout.rest[offset] ?? 0,
      layout.rest[offset + 1] ?? 0,
      layout.rest[offset + 2] ?? 0,
      view,
      geo,
    );
    if (arc !== null) {
      traceArc(context, geo, arc);
      context.fill();
    }
  }

  context.lineWidth = 1;
  context.strokeStyle = theme.background;
  for (const index of drawn) {
    const arc = arcOf(layout, index, view, geo);
    if (arc === null) {
      continue;
    }
    context.globalAlpha = path === null || path.has(index) ? 1 : DIMMED;
    context.fillStyle = layout.leaf[index] === 1 ? sunColor(layout, index) : theme.directory;
    traceArc(context, geo, arc);
    context.fill();
    context.stroke();
  }
  context.globalAlpha = 1;

  // Labels: radial, on arcs wide enough for a line of text.
  context.font = `${LABEL_FONT_SIZE}px ${theme.font}`;
  context.textBaseline = "middle";
  for (const index of drawn) {
    const arc = arcOf(layout, index, view, geo);
    if (arc === null || arc.inner <= 0) {
      continue;
    }
    const middle = (arc.inner + arc.outer) / 2;
    if ((arc.end - arc.start) * middle < LABEL_FONT_SIZE + 3) {
      continue;
    }
    const text = fitLabel(layout.names[index] ?? "", arc.outer - arc.inner, LABEL_FONT_SIZE);
    if (text === null) {
      continue;
    }
    const angle = (arc.start + arc.end) / 2 - Math.PI / 2;
    const flipped = Math.cos(angle) < 0;
    context.save();
    context.translate(geo.cx, geo.cy);
    context.rotate(flipped ? angle + Math.PI : angle);
    context.textAlign = flipped ? "right" : "left";
    context.globalAlpha = path === null || path.has(index) ? 1 : DIMMED;
    context.fillStyle = layout.leaf[index] === 1 ? theme.leafText : theme.text;
    context.fillText(text, flipped ? -arc.inner - 3 : arc.inner + 3, 0, arc.outer - arc.inner - 6);
    context.restore();
  }
  context.globalAlpha = 1;
  context.textAlign = "left";

  // The centre names what the circle is.
  const centre = drawn.find((index) => (layout.depth[index] ?? 0) === Math.round(view.d0));
  if (centre !== undefined && geo.ring >= 48) {
    context.fillStyle = theme.text;
    context.textAlign = "center";
    const name = fitLabel(layout.names[centre] ?? "", geo.ring * 1.8, LABEL_FONT_SIZE + 1);
    context.font = `600 ${LABEL_FONT_SIZE + 1}px ${theme.font}`;
    if (name !== null) {
      context.fillText(name, geo.cx, geo.cy - 8);
    }
    context.font = `${LABEL_FONT_SIZE}px ${theme.font}`;
    context.fillText(formatBytes(layout.value[centre] ?? 0), geo.cx, geo.cy + 8);
    context.textAlign = "left";
  }

  const outline = (index: number, color: string, lineWidth: number) => {
    const arc = arcOf(layout, index, view, geo);
    if (arc === null) {
      return;
    }
    context.strokeStyle = color;
    context.lineWidth = lineWidth;
    traceArc(context, geo, arc);
    context.stroke();
  };
  if (selected !== null) {
    outline(selected, theme.highlight, 3);
  }
  if (hovered !== null && hovered !== selected) {
    outline(hovered, theme.text, 1.5);
  }
}

export default function Sunburst({
  root,
  selectedId = null,
  onActivate,
  onReveal,
  onSelectionChange,
  onHover,
  onBack,
  enterFrom = null,
  label,
  ref,
}: SunburstProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [size, setSize] = useState({ width: 0, height: 0, dpr: 1 });

  const viewRef = useRef<SunView>(FULL_VIEW);
  const drawnRef = useRef<Uint32Array | null>(null);
  const hoveredRef = useRef<number | null>(null);
  const pendingRef = useRef<number | null>(null);
  const transitionRef = useRef<Transition | null>(null);

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

  // The expensive step, once per tree. The size only changes the geometry.
  const layout = useMemo(() => (root === null ? null : layoutSunburst(root)), [root]);

  const selected = layout === null || selectedId === null ? null : indexOfIdSun(layout, selectedId);
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
        drawnRef.current = null;
        return;
      }

      const transition = transitionRef.current;
      if (transition !== null) {
        const t = transition.duration <= 0 ? 1 : (now - transition.start) / transition.duration;
        viewRef.current = interpolateSun(transition.from, transition.to, easeInOut(t));
        if (t >= 1) {
          transitionRef.current = null;
          transition.done();
        } else {
          scheduleRef.current();
        }
      }

      const view = viewRef.current;
      const geo = geometry(layout, view, size.width, size.height);
      const drawn = visibleArcs(layout, view, geo);
      drawnRef.current = drawn;
      draw(
        context,
        layout,
        drawn,
        view,
        geo,
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

  const stopTransition = useCallback(() => {
    const transition = transitionRef.current;
    transitionRef.current = null;
    transition?.done();
  }, []);

  const animate = useCallback(
    (from: SunView, to: SunView): Promise<void> => {
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
        const index = layout === null ? null : indexOfIdSun(layout, id);
        if (layout === null || index === null) {
          return Promise.resolve();
        }
        return animate(viewRef.current, fitSun(layout, index));
      },
    }),
    [animate, layout],
  );

  const enterFromRef = useRef(enterFrom);
  enterFromRef.current = enterFrom;
  useEffect(() => {
    stopTransition();
    hoveredRef.current = null;
    viewRef.current = FULL_VIEW;
    const from = enterFromRef.current;
    const index = layout === null || from === null ? null : indexOfIdSun(layout, from);
    if (layout !== null && index !== null && !reducedMotion()) {
      viewRef.current = fitSun(layout, index);
      void animate(viewRef.current, FULL_VIEW);
    }
    schedule();
  }, [layout, schedule, animate, stopTransition]);

  useEffect(() => {
    schedule();
  }, [selected, schedule]);

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

  useEffect(() => {
    const query = window.matchMedia("(prefers-color-scheme: dark)");
    query.addEventListener("change", schedule);
    return () => query.removeEventListener("change", schedule);
  }, [schedule]);

  const indexAt = (event: PointerEvent<HTMLCanvasElement> | MouseEvent<HTMLCanvasElement>): number | null => {
    const drawn = drawnRef.current;
    if (layout === null || drawn === null) {
      return null;
    }
    const box = event.currentTarget.getBoundingClientRect();
    const view = viewRef.current;
    return hitTestSunburst(
      layout,
      drawn,
      view,
      geometry(layout, view, size.width, size.height),
      event.clientX - box.left,
      event.clientY - box.top,
    );
  };

  const onPointerMove = (event: PointerEvent<HTMLCanvasElement>) => {
    const hovered = indexAt(event);
    if (hovered !== hoveredRef.current) {
      hoveredRef.current = hovered;
      schedule();
      onHover?.(hovered === null || layout === null ? null : hitOf(layout, hovered));
    }
  };

  const onPointerLeave = () => {
    if (hoveredRef.current !== null) {
      hoveredRef.current = null;
      schedule();
      onHover?.(null);
    }
  };

  const onClick = (event: MouseEvent<HTMLCanvasElement>) => {
    if (event.detail > 1 || layout === null) {
      return;
    }
    const index = indexAt(event);
    if (index === null) {
      return;
    }
    // The centre is the folder being shown: clicking it goes up.
    if ((layout.depth[index] ?? 0) <= Math.round(viewRef.current.d0)) {
      onBack?.();
      return;
    }
    onActivate?.(hitOf(layout, index), "pointer");
  };

  const onDoubleClick = (event: MouseEvent<HTMLCanvasElement>) => {
    const index = indexAt(event);
    if (index !== null && layout !== null && index !== 0) {
      onReveal?.(hitOf(layout, index));
    }
  };

  const onKeyDown = (event: KeyboardEvent<HTMLCanvasElement>) => {
    if (layout === null) {
      return;
    }
    const direction = KEYS[event.key];
    const modified = event.metaKey || event.ctrlKey || event.altKey;
    if (direction !== undefined && !modified) {
      event.preventDefault();
      const next = neighborSun(layout, selectedRef.current, direction);
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
      } else if (selectedRef.current === 0) {
        onBack?.();
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
        role="application"
        aria-roledescription="storage sunburst"
        tabIndex={0}
        aria-label={label}
        style={{ width: size.width, height: size.height }}
        onPointerMove={onPointerMove}
        onPointerLeave={onPointerLeave}
        onClick={onClick}
        onDoubleClick={onDoubleClick}
        onKeyDown={onKeyDown}
      />
    </div>
  );
}

/**
 * Zoomable, scrollable waveform with draggable regions — the canvas half of
 * the annotation editor. The view lives in a real horizontally-scrolling box
 * (so the trackpad and the scrollbar just work) with the canvas stuck to its
 * left edge; wheel over the wave zooms around the pointer.
 */

import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import type { Annotation } from "../../src/listen/types";
import { peaksFor, type PeakCache } from "./peaks";

export interface WaveHandle {
  /** Scroll so `t` is visible (centred if it was off-screen). */
  reveal(t: number): void;
  /** Multiply the zoom, anchored on the playhead when visible, else the centre. */
  zoomBy(factor: number): void;
  fit(): void;
}

interface Props {
  peaks: PeakCache | null;
  duration: number;
  position: number;
  playing: boolean;
  annotations: Annotation[];
  labels: string[];
  selectedId: string | null;
  selection: [number, number] | null;
  /** Vertical magnification of the trace (breaths are quiet). */
  gain: number;
  onSeek: (t: number) => void;
  onSelection: (sel: [number, number] | null) => void;
  onSelect: (id: string | null) => void;
  onChange: (a: Annotation) => void;
}

const RULER = 16;
const HEIGHT = 190;
const EDGE_PX = 5;
const MAX_PPS = 20000;

/** Label → colour, by position in the label set. */
export const LABEL_COLOURS = [
  [70, 160, 90],
  [235, 140, 30],
  [150, 90, 210],
  [40, 160, 190],
  [130, 130, 130],
  [220, 60, 120],
  [180, 170, 30],
  [90, 110, 220],
];
export const labelColour = (labels: string[], label: string): number[] => {
  const i = labels.indexOf(label);
  return LABEL_COLOURS[(i < 0 ? LABEL_COLOURS.length - 1 : i) % LABEL_COLOURS.length];
};

export const fmtTime = (s: number): string => {
  const m = Math.floor(s / 60);
  const r = s - m * 60;
  return `${m}:${r.toFixed(3).padStart(6, "0")}`;
};

const tickStep = (pps: number): number => {
  for (const s of [0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600])
    if (s * pps >= 70) return s;
  return 1200;
};

type Drag =
  | { kind: "select"; t0: number; x0: number; moved: boolean }
  | { kind: "move"; id: string; t0: number; x0: number; start: number; end: number; moved: boolean }
  | { kind: "resize"; id: string; edge: "start" | "end" };

export const AnnotateWave = forwardRef<WaveHandle, Props>(function AnnotateWave(
  { peaks, duration, position, playing, annotations, labels, selectedId, selection, gain, onSeek, onSelection, onSelect, onChange },
  ref,
) {
  const scroller = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [width, setWidth] = useState(0);
  const [pps, setPps] = useState(0); // pixels per second; 0 = not yet fitted
  const ppsLive = useRef(pps); // zoom steps can arrive faster than renders
  ppsLive.current = pps;
  const [scrollLeft, setScrollLeft] = useState(0);
  const wantScroll = useRef<number | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [cursor, setCursor] = useState("crosshair");

  const minPps = duration > 0 && width > 0 ? width / duration : 0;
  const viewStart = pps > 0 ? scrollLeft / pps : 0;
  const viewEnd = pps > 0 ? viewStart + width / pps : 0;

  // Track our own width.
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    setWidth(el.clientWidth);
    return () => ro.disconnect();
  }, []);

  // Fit the whole file once we know how wide we are.
  useEffect(() => {
    if (pps === 0 && minPps > 0) setPps(minPps);
    else if (pps > 0 && pps < minPps) setPps(minPps);
  }, [minPps, pps]);

  // Zoom changes the spacer width; apply the requested scroll after that renders.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || wantScroll.current == null) return;
    el.scrollLeft = wantScroll.current;
    wantScroll.current = null;
    setScrollLeft(el.scrollLeft);
  });

  const zoomAt = useCallback(
    (factor: number, anchorT: number, anchorX: number) => {
      const cur = ppsLive.current;
      const next = Math.max(minPps, Math.min(MAX_PPS, cur * factor));
      if (next === cur) return;
      wantScroll.current = Math.max(0, anchorT * next - anchorX);
      ppsLive.current = next;
      setPps(next);
    },
    [minPps],
  );

  const scrollTo = useCallback((left: number) => {
    const el = scroller.current;
    if (!el) return;
    el.scrollLeft = Math.max(0, left);
    setScrollLeft(el.scrollLeft);
  }, []);

  useImperativeHandle(
    ref,
    () => ({
      reveal(t) {
        if (pps <= 0) return;
        if (t >= viewStart && t <= viewEnd) return;
        scrollTo(t * pps - width / 2);
      },
      zoomBy(factor) {
        const t = position >= viewStart && position <= viewEnd ? position : (viewStart + viewEnd) / 2;
        zoomAt(factor, t, (t - viewStart) * pps);
      },
      fit() {
        wantScroll.current = 0;
        setPps(minPps);
      },
    }),
    [minPps, position, pps, scrollTo, viewEnd, viewStart, width, zoomAt],
  );

  // Wheel: vertical delta (or pinch) zooms around the pointer; horizontal scrolls natively.
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const onWheel = (e: WheelEvent): void => {
      if (Math.abs(e.deltaY) <= Math.abs(e.deltaX) && !e.ctrlKey) return;
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const cur = ppsLive.current;
      const t = (el.scrollLeft + x) / cur;
      zoomAt(Math.exp(-e.deltaY * (e.ctrlKey ? 0.01 : 0.002)), t, x);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [zoomAt]);

  // Follow the playhead while playing.
  useEffect(() => {
    if (!playing || pps <= 0) return;
    if (position > viewEnd || position < viewStart) scrollTo(position * pps - 20);
  }, [playing, position, pps, scrollTo, viewEnd, viewStart]);

  // ---- drawing ----
  useEffect(() => {
    const el = canvas.current;
    if (!el || width === 0 || pps <= 0) return;
    const dpr = window.devicePixelRatio || 1;
    const w = width, h = HEIGHT;
    if (el.width !== w * dpr || el.height !== h * dpr) {
      el.width = w * dpr;
      el.height = h * dpr;
    }
    const g = el.getContext("2d")!;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, w, h);
    const tx = (t: number): number => (t - viewStart) * pps;

    // Ruler
    g.fillStyle = "rgba(255,255,255,0.55)";
    g.fillRect(0, 0, w, RULER);
    const step = tickStep(pps);
    g.font = "9px Monaco, Menlo, monospace";
    g.fillStyle = "#555";
    g.strokeStyle = "rgba(0,0,0,0.25)";
    g.lineWidth = 1;
    g.beginPath();
    for (let t = Math.floor(viewStart / step) * step; t <= viewEnd + step; t += step) {
      const x = Math.round(tx(t)) + 0.5;
      g.moveTo(x, RULER - 5);
      g.lineTo(x, RULER);
      g.moveTo(x, RULER);
      g.lineTo(x, h);
    }
    g.stroke();
    for (let t = Math.floor(viewStart / step) * step; t <= viewEnd + step; t += step) {
      g.fillText(step < 1 ? fmtTime(t) : fmtTime(t).replace(/\.\d+$/, ""), tx(t) + 3, 10);
    }

    // Regions
    const laneY = RULER, laneH = 14;
    for (const a of annotations) {
      if (a.end < viewStart || a.start > viewEnd) continue;
      const x0 = tx(a.start), x1 = Math.max(tx(a.end), x0 + 1);
      const [r, gg, b] = labelColour(labels, a.label);
      const sel = a.id === selectedId;
      g.fillStyle = `rgba(${r},${gg},${b},${sel ? 0.32 : 0.18})`;
      g.fillRect(x0, RULER, x1 - x0, h - RULER);
      g.fillStyle = `rgba(${r},${gg},${b},${sel ? 0.95 : 0.7})`;
      g.fillRect(x0, laneY, x1 - x0, laneH);
      g.strokeStyle = `rgba(${r},${gg},${b},${sel ? 1 : 0.8})`;
      g.lineWidth = sel ? 2 : 1;
      g.setLineDash(a.confidence === "maybe" ? [3, 3] : []);
      g.beginPath();
      g.moveTo(Math.round(x0) + 0.5, RULER);
      g.lineTo(Math.round(x0) + 0.5, h);
      g.moveTo(Math.round(x1) - 0.5, RULER);
      g.lineTo(Math.round(x1) - 0.5, h);
      g.stroke();
      g.setLineDash([]);
      if (x1 - x0 > 18) {
        g.save();
        g.beginPath();
        g.rect(x0, laneY, x1 - x0, laneH);
        g.clip();
        g.fillStyle = "#fff";
        g.font = "bold 10px 'Lucida Grande', Helvetica, sans-serif";
        g.fillText(`${a.label}${a.confidence === "maybe" ? "?" : ""}`, x0 + 3, laneY + 10.5);
        g.restore();
      }
    }

    // Selection
    if (selection) {
      g.fillStyle = "rgba(60,130,246,0.2)";
      g.fillRect(tx(selection[0]), RULER, tx(selection[1]) - tx(selection[0]), h - RULER);
      g.strokeStyle = "rgba(40,100,220,0.9)";
      g.lineWidth = 1;
      g.beginPath();
      g.moveTo(Math.round(tx(selection[0])) + 0.5, RULER);
      g.lineTo(Math.round(tx(selection[0])) + 0.5, h);
      g.moveTo(Math.round(tx(selection[1])) + 0.5, RULER);
      g.lineTo(Math.round(tx(selection[1])) + 0.5, h);
      g.stroke();
    }

    // Waveform
    if (peaks) {
      const sr = peaks.buffer.sampleRate;
      const spp = sr / pps;
      const { min, max } = peaksFor(peaks, viewStart * sr, spp, w);
      const top = RULER + laneH, mid = top + (h - top) / 2, half = (h - top) / 2 - 2;
      g.strokeStyle = "rgba(0,0,0,0.12)";
      g.beginPath();
      g.moveTo(0, mid + 0.5);
      g.lineTo(w, mid + 0.5);
      g.stroke();
      g.beginPath();
      for (let x = 0; x < w; x++) {
        const hi = Math.min(1, max[x] * gain), lo = Math.max(-1, min[x] * gain);
        const y0 = mid - hi * half, y1 = mid - lo * half;
        g.moveTo(x + 0.5, y0);
        g.lineTo(x + 0.5, Math.max(y1, y0 + 1));
      }
      g.strokeStyle = "rgba(40,60,90,0.8)";
      g.lineWidth = 1;
      g.stroke();
    }

    // Playhead
    if (position >= viewStart && position <= viewEnd) {
      g.fillStyle = "rgba(220,40,40,0.9)";
      g.fillRect(tx(position) - 0.5, 0, 1.5, h);
    }
  }, [annotations, gain, labels, peaks, position, pps, selectedId, selection, viewEnd, viewStart, width]);

  // ---- pointer interaction ----
  const timeAt = (clientX: number): number => {
    const rect = canvas.current!.getBoundingClientRect();
    return Math.max(0, Math.min(duration, viewStart + (clientX - rect.left) / pps));
  };
  const xAt = (clientX: number): number => clientX - canvas.current!.getBoundingClientRect().left;

  /** Which region edge or body sits under x? Smallest region wins when nested. */
  type Hit = { a: Annotation; part: "start" | "end" | "body" };
  const hit = (x: number): Hit | null => {
    const edgeOf = (a: Annotation): "start" | "end" | null => {
      if (Math.abs(x - (a.start - viewStart) * pps) <= EDGE_PX) return "start";
      if (Math.abs(x - (a.end - viewStart) * pps) <= EDGE_PX) return "end";
      return null;
    };
    // The selected region gets first dibs on its edges.
    const sel = annotations.find((a) => a.id === selectedId);
    const selEdge = sel && edgeOf(sel);
    if (sel && selEdge) return { a: sel, part: selEdge };
    let edge: Hit | null = null, body: Hit | null = null;
    for (const a of annotations) {
      const e = edgeOf(a);
      if (e && !edge) edge = { a, part: e };
      const x0 = (a.start - viewStart) * pps, x1 = (a.end - viewStart) * pps;
      if (x > x0 && x < x1 && (!body || a.end - a.start < body.a.end - body.a.start)) body = { a, part: "body" };
    }
    return edge ?? body;
  };

  const onPointerDown = (e: React.PointerEvent<HTMLCanvasElement>): void => {
    if (e.button !== 0 || pps <= 0) return;
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* synthetic events have no pointer to capture */
    }
    const x = xAt(e.clientX), t = timeAt(e.clientX);
    const h = hit(x);
    if (h && h.part !== "body") {
      onSelect(h.a.id);
      onSelection(null);
      setDrag({ kind: "resize", id: h.a.id, edge: h.part });
    } else if (h) {
      onSelect(h.a.id);
      onSelection(null);
      setDrag({ kind: "move", id: h.a.id, t0: t, x0: x, start: h.a.start, end: h.a.end, moved: false });
    } else {
      setDrag({ kind: "select", t0: t, x0: x, moved: false });
    }
  };

  const onPointerMove = (e: React.PointerEvent<HTMLCanvasElement>): void => {
    if (pps <= 0) return;
    const x = xAt(e.clientX), t = timeAt(e.clientX);
    if (!drag) {
      const h = hit(x);
      setCursor(!h ? "crosshair" : h.part === "body" ? "grab" : "col-resize");
      return;
    }
    if (drag.kind === "select") {
      if (!drag.moved && Math.abs(x - drag.x0) < 4) return;
      if (!drag.moved) setDrag({ ...drag, moved: true });
      onSelection([Math.min(drag.t0, t), Math.max(drag.t0, t)]);
    } else if (drag.kind === "move") {
      if (!drag.moved && Math.abs(x - drag.x0) < 3) return;
      if (!drag.moved) setDrag({ ...drag, moved: true });
      const a = annotations.find((r) => r.id === drag.id);
      if (!a) return;
      const len = drag.end - drag.start;
      const start = Math.max(0, Math.min(duration - len, drag.start + (t - drag.t0)));
      onChange({ ...a, start, end: start + len });
    } else {
      const a = annotations.find((r) => r.id === drag.id);
      if (!a) return;
      if (drag.edge === "start") onChange({ ...a, start: Math.min(t, a.end - 0.005) });
      else onChange({ ...a, end: Math.max(t, a.start + 0.005) });
    }
  };

  const onPointerUp = (e: React.PointerEvent<HTMLCanvasElement>): void => {
    if (!drag) return;
    setDrag(null);
    if (drag.kind === "select") {
      const t = timeAt(e.clientX);
      if (!drag.moved) {
        onSelection(null);
        onSelect(null);
        onSeek(t);
      } else {
        const a = Math.min(drag.t0, t), b = Math.max(drag.t0, t);
        onSelection([a, b]);
        onSeek(a);
      }
    } else if (drag.kind === "move" && !drag.moved) {
      onSeek(drag.start);
    }
  };

  return (
    <div className="annot-wave">
      <div className="annot-scroll" ref={scroller} onScroll={(e) => setScrollLeft(e.currentTarget.scrollLeft)}>
        <div className="annot-spacer" style={{ width: pps > 0 ? Math.max(width, duration * pps) : "100%" }}>
          <canvas
            ref={canvas}
            className="annot-canvas"
            style={{ width, height: HEIGHT, cursor: drag ? (drag.kind === "move" ? "grabbing" : cursor) : cursor }}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerCancel={() => setDrag(null)}
          />
        </div>
      </div>
      <div className="annot-zoom hint small">
        {fmtTime(viewStart)} – {fmtTime(Math.min(viewEnd, duration))} · {pps > 0 ? `${(pps / Math.max(minPps, 1e-9)).toFixed(1)}×` : ""}
      </div>
    </div>
  );
});

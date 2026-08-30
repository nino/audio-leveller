import { useEffect, useRef, useState } from "react";

interface Props {
  /** Sample snapshot, taken at decode time (see Trial.tsx) – never a live AudioBuffer. */
  data: Float32Array | null;
  position: number;
  duration: number;
  region: [number, number] | null;
  onSeek: (pos: number) => void;
  onRegion: (r: [number, number] | null) => void;
}

/** Waveform overview: click to seek, drag to set a loop region. */
export function Waveform({ data, position, duration, region, onSeek, onRegion }: Props) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [drag, setDrag] = useState<{ from: number; to: number } | null>(null);

  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    const dpr = window.devicePixelRatio || 1;
    const w = el.clientWidth, h = el.clientHeight;
    el.width = w * dpr;
    el.height = h * dpr;
    const g = el.getContext("2d")!;
    g.scale(dpr, dpr);
    g.clearRect(0, 0, w, h);

    // Region / drag highlight
    const sel = drag ? [Math.min(drag.from, drag.to), Math.max(drag.from, drag.to)] : region;
    if (sel && duration > 0) {
      g.fillStyle = "rgba(60, 130, 246, 0.18)";
      g.fillRect((sel[0] / duration) * w, 0, ((sel[1] - sel[0]) / duration) * w, h);
    }

    if (data) {
      const step = Math.max(1, Math.floor(data.length / w));
      g.beginPath();
      for (let x = 0; x < w; x++) {
        let lo = 1, hi = -1;
        const s0 = x * step, s1 = Math.min(data.length, s0 + step);
        for (let i = s0; i < s1; i++) {
          const v = data[i];
          if (v < lo) lo = v;
          if (v > hi) hi = v;
        }
        g.moveTo(x + 0.5, (1 - hi) * 0.5 * h);
        g.lineTo(x + 0.5, (1 - lo) * 0.5 * h);
      }
      g.strokeStyle = "rgba(40, 60, 90, 0.75)";
      g.lineWidth = 1;
      g.stroke();
    }

    if (duration > 0) {
      const x = (position / duration) * w;
      g.fillStyle = "rgba(220, 40, 40, 0.9)";
      g.fillRect(x - 0.5, 0, 1.5, h);
    }
  }, [data, position, duration, region, drag]);

  const posAt = (e: React.MouseEvent): number => {
    const r = canvas.current!.getBoundingClientRect();
    return Math.max(0, Math.min(duration, ((e.clientX - r.left) / r.width) * duration));
  };

  return (
    <canvas
      ref={canvas}
      className="waveform"
      onMouseDown={(e) => setDrag({ from: posAt(e), to: posAt(e) })}
      onMouseMove={(e) => drag && setDrag({ ...drag, to: posAt(e) })}
      onMouseUp={(e) => {
        if (!drag) return;
        const to = posAt(e);
        const a = Math.min(drag.from, to), b = Math.max(drag.from, to);
        setDrag(null);
        if (b - a < 0.15) onSeek(a);
        else {
          onRegion([a, b]);
          onSeek(a);
        }
      }}
      onMouseLeave={() => setDrag(null)}
    />
  );
}

/**
 * Gapless A/B/C player. Every clip of a trial is decoded up front; one shared
 * playhead runs across them, so switching clips mid-word keeps the position
 * exactly – the only way to hear small differences.
 */

import { useCallback, useEffect, useRef, useState } from "react";

export interface PlayerState {
  loading: boolean;
  error: string | null;
  duration: number;
  position: number;
  playing: boolean;
  current: number;
  loop: boolean;
  /** Loop region as [start, end] in seconds, or null for the whole clip. */
  region: [number, number] | null;
}

let ctx: AudioContext | null = null;
const audioContext = (): AudioContext => {
  if (!ctx) ctx = new AudioContext();
  return ctx;
};

export function usePlayer(urls: string[]) {
  const [buffers, setBuffers] = useState<AudioBuffer[]>([]);
  const [state, setState] = useState<PlayerState>({
    loading: true,
    error: null,
    duration: 0,
    position: 0,
    playing: false,
    current: 0,
    loop: true,
    region: null,
  });

  const source = useRef<AudioBufferSourceNode | null>(null);
  const gain = useRef<GainNode | null>(null);
  const startedAt = useRef(0); // ctx.currentTime when playback (re)started
  const startPos = useRef(0); // clip position at that moment
  const raf = useRef(0);
  const live = useRef(state);
  live.current = state;

  // Decode all clips whenever the URL list changes.
  useEffect(() => {
    let cancelled = false;
    setState((s) => ({ ...s, loading: true, error: null, playing: false, position: 0, current: 0, region: null }));
    stopSource();
    (async () => {
      try {
        const decoded = await Promise.all(
          urls.map(async (u) => {
            const res = await fetch(u);
            if (!res.ok) throw new Error(`clip ${u}: ${res.status}`);
            return audioContext().decodeAudioData(await res.arrayBuffer());
          }),
        );
        if (cancelled) return;
        setBuffers(decoded);
        setState((s) => ({ ...s, loading: false, duration: decoded[0]?.duration ?? 0 }));
      } catch (err) {
        if (!cancelled) setState((s) => ({ ...s, loading: false, error: String(err) }));
      }
    })();
    return () => {
      cancelled = true;
      stopSource();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [urls.join("|")]);

  function stopSource(): void {
    if (source.current) {
      try {
        source.current.onended = null;
        source.current.stop();
      } catch {
        /* already stopped */
      }
      source.current.disconnect();
      source.current = null;
    }
    cancelAnimationFrame(raf.current);
  }

  const currentPosition = useCallback((): number => {
    const s = live.current;
    if (!s.playing) return s.position;
    const [lo, hi] = s.region ?? [0, s.duration];
    const elapsed = audioContext().currentTime - startedAt.current;
    let p = startPos.current + elapsed;
    if (s.loop && hi > lo) p = lo + ((p - lo) % (hi - lo));
    return Math.min(p, s.duration);
  }, []);

  /** Start (or restart) the current clip from `pos`. */
  const startAt = useCallback(
    (index: number, pos: number) => {
      const buf = buffers[index];
      if (!buf) return;
      stopSource();
      const ac = audioContext();
      if (ac.state === "suspended") void ac.resume();
      if (!gain.current) {
        gain.current = ac.createGain();
        gain.current.connect(ac.destination);
      }
      const s = live.current;
      const [lo, hi] = s.region ?? [0, buf.duration];
      const node = ac.createBufferSource();
      node.buffer = buf;
      node.loop = s.loop;
      node.loopStart = lo;
      node.loopEnd = hi;
      // A tiny fade-in avoids a click when switching mid-waveform.
      gain.current.gain.cancelScheduledValues(ac.currentTime);
      gain.current.gain.setValueAtTime(0, ac.currentTime);
      gain.current.gain.linearRampToValueAtTime(1, ac.currentTime + 0.004);
      node.connect(gain.current);
      const clamped = Math.max(lo, Math.min(pos, hi - 0.001));
      node.start(0, clamped);
      startedAt.current = ac.currentTime;
      startPos.current = clamped;
      source.current = node;
      node.onended = () => {
        if (source.current !== node) return;
        source.current = null;
        setState((st) => ({ ...st, playing: false, position: st.region?.[0] ?? 0 }));
      };
      const tick = (): void => {
        setState((st) => ({ ...st, position: currentPosition() }));
        raf.current = requestAnimationFrame(tick);
      };
      raf.current = requestAnimationFrame(tick);
    },
    [buffers, currentPosition],
  );

  const play = useCallback(() => {
    if (live.current.loading || buffers.length === 0) return;
    const pos = live.current.position;
    setState((s) => ({ ...s, playing: true }));
    live.current = { ...live.current, playing: true };
    startAt(live.current.current, pos);
  }, [buffers, startAt]);

  const pause = useCallback(() => {
    const pos = currentPosition();
    stopSource();
    setState((s) => ({ ...s, playing: false, position: pos }));
  }, [currentPosition]);

  const toggle = useCallback(() => (live.current.playing ? pause() : play()), [pause, play]);

  const select = useCallback(
    (index: number) => {
      if (index < 0 || index >= buffers.length) return;
      const pos = currentPosition();
      setState((s) => ({ ...s, current: index, position: pos }));
      live.current = { ...live.current, current: index, position: pos };
      if (live.current.playing) startAt(index, pos);
    },
    [buffers.length, currentPosition, startAt],
  );

  const seek = useCallback(
    (pos: number) => {
      const p = Math.max(0, Math.min(pos, live.current.duration));
      setState((s) => ({ ...s, position: p }));
      live.current = { ...live.current, position: p };
      if (live.current.playing) startAt(live.current.current, p);
    },
    [startAt],
  );

  const setLoop = useCallback(
    (loop: boolean) => {
      const pos = currentPosition();
      setState((s) => ({ ...s, loop, position: pos }));
      live.current = { ...live.current, loop, position: pos };
      if (live.current.playing) startAt(live.current.current, pos);
    },
    [currentPosition, startAt],
  );

  const setRegion = useCallback(
    (region: [number, number] | null) => {
      const pos = currentPosition();
      const p = region ? Math.max(region[0], Math.min(pos, region[1])) : pos;
      setState((s) => ({ ...s, region, position: p }));
      live.current = { ...live.current, region, position: p };
      if (live.current.playing) startAt(live.current.current, p);
    },
    [currentPosition, startAt],
  );

  return { state, buffers, play, pause, toggle, select, seek, setLoop, setRegion };
}

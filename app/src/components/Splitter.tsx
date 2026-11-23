import { useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import { clampPane, type Pane } from "../lib/layout";

const STEP = 16;

/**
 * A drag handle that resizes one pane. `sign` is +1 when moving right/down grows the pane and -1
 * when moving left/up grows it. `measure` reads the pane's current size, `room` the largest size
 * it may take before squeezing the map (or the window). Double-click resets to the default.
 */
export function Splitter({ pane, orientation, sign, label, measure, room, onResize, className = "" }: {
  pane: Pane;
  orientation: "vertical" | "horizontal";
  sign: 1 | -1;
  label: string;
  measure: () => number;
  room: () => number;
  onResize: (size: number | null) => void;
  className?: string;
}) {
  const drag = useRef<{ origin: number; start: number; room: number } | null>(null);
  const [value, setValue] = useState<number | undefined>();
  const [active, setActive] = useState(false);
  const vertical = orientation === "vertical";
  const viewport = () => (vertical ? window.innerWidth : window.innerHeight);

  const apply = (size: number, max: number) => {
    const s = clampPane(pane, size, viewport(), max);
    setValue(s);
    onResize(s);
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    const start = measure();
    drag.current = { origin: vertical ? e.clientX : e.clientY, start, room: room() };
    setValue(Math.round(start));
    setActive(true);
    document.body.classList.add(vertical ? "resizing-col" : "resizing-row");
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    const delta = (vertical ? e.clientX : e.clientY) - d.origin;
    apply(d.start + sign * delta, d.room);
  };
  const end = (e: PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    drag.current = null;
    setActive(false);
    if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
    document.body.classList.remove("resizing-col", "resizing-row");
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const grow: Record<string, number> = vertical ? { ArrowRight: sign, ArrowLeft: -sign } : { ArrowDown: sign, ArrowUp: -sign };
    const dir = grow[e.key];
    if (dir) { e.preventDefault(); apply(measure() + dir * STEP, room()); }
    else if (e.key === "Home" || e.key === "Enter") { e.preventDefault(); reset(); }
  };
  const reset = () => { setValue(undefined); onResize(null); };

  return (
    <div
      className={`splitter ${vertical ? "col" : "row"} ${active ? "active" : ""} ${className}`}
      role="separator" tabIndex={0} aria-label={label} aria-orientation={vertical ? "vertical" : "horizontal"}
      aria-valuenow={value ?? undefined} aria-valuemin={clampPane(pane, 0, viewport())}
      title={`${label} — drag to resize, double-click to reset`}
      data-testid={`splitter-${pane}`}
      onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={end} onPointerCancel={end}
      onLostPointerCapture={end} onDoubleClick={reset} onKeyDown={onKeyDown}
      onFocus={() => setValue(Math.round(measure()))}
    />
  );
}

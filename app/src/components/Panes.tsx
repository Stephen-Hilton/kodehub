import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { MIN_MAP, loadSizes, paneVars, saveSizes, type Pane, type PaneSizes } from "../lib/layout";
import { Splitter } from "./Splitter";

/** Least height left to the map row when the terminal is dragged taller. */
const MIN_MAIN_H = 160;

/** User-sized panes, persisted. `setPane(p, null)` returns a pane to its responsive default. */
export function usePaneLayout() {
  const [sizes, setSizes] = useState<PaneSizes>(loadSizes);
  const mainRef = useRef<HTMLDivElement>(null);
  useEffect(() => { saveSizes(sizes); }, [sizes]);
  const setPane = useCallback((p: Pane, v: number | null) => setSizes((s) => {
    const next = { ...s };
    if (v == null) delete next[p]; else next[p] = v;
    return next;
  }), []);
  return {
    sizes, setPane, mainRef,
    mainStyle: paneVars(sizes, ["left", "right", "panel"]),
    appStyle: paneVars(sizes, ["term"]),
  };
}

/** Resolved pixel widths of the main grid's tracks: [left, map, (panel), right]. */
function tracks(main: HTMLElement | null): number[] {
  if (!main) return [];
  return getComputedStyle(main).gridTemplateColumns.split(/\s+/).map((t) => parseFloat(t) || 0);
}

/** Column handles for the main row: requirements | map | (node panel) | agents. */
export function MainSplitters({ mainRef, panel, setPane }: {
  mainRef: RefObject<HTMLDivElement | null>;
  panel: boolean;
  setPane: (p: Pane, v: number | null) => void;
}) {
  const index = (p: Pane) => {
    const n = tracks(mainRef.current).length;
    return p === "left" ? 0 : p === "panel" ? 2 : n - 1;
  };
  const measure = (p: Pane) => () => tracks(mainRef.current)[index(p)] ?? 0;
  // a pane may grow until the map is down to MIN_MAP
  const room = (p: Pane) => () => {
    const t = tracks(mainRef.current);
    return (t[index(p)] ?? 0) + Math.max(0, (t[1] ?? 0) - MIN_MAP);
  };
  const props = (p: Pane) => ({ pane: p, measure: measure(p), room: room(p), onResize: (v: number | null) => setPane(p, v), orientation: "vertical" as const });
  return (
    <>
      <Splitter {...props("left")} sign={1} label="Resize requirements panel" className="at-left" />
      {panel && <Splitter {...props("panel")} sign={-1} label="Resize node panel" className="at-panel" />}
      <Splitter {...props("right")} sign={-1} label="Resize agents panel" className="at-right" />
    </>
  );
}

/** Row handle on top of the terminal dock. */
export function TerminalSplitter({ setPane }: { setPane: (p: Pane, v: number | null) => void }) {
  const dock = () => document.querySelector<HTMLElement>("[data-testid=terminal]");
  const main = () => document.querySelector<HTMLElement>(".main");
  return (
    <Splitter pane="term" orientation="horizontal" sign={-1} label="Resize terminal"
      measure={() => dock()?.offsetHeight ?? 280}
      room={() => (dock()?.offsetHeight ?? 280) + Math.max(0, (main()?.offsetHeight ?? 0) - MIN_MAIN_H)}
      onResize={(v) => setPane("term", v)} />
  );
}

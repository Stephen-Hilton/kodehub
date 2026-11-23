// Turns map data into Cytoscape elements with semantic zoom (doc 05): levels
// Context → Container → Component → Symbol. Pure (unit tested).
import type { MapData, MapNode } from "./api";

export const LEVELS = ["context", "container", "component", "symbol"] as const;
export type Level = (typeof LEVELS)[number];

export function levelIndex(l: string): number {
  return LEVELS.indexOf(l as Level);
}

/** Depth to show for a zoom factor (auto semantic zoom). */
export function depthForZoom(zoom: number): number {
  if (zoom < 0.45) return 1;
  if (zoom < 0.7) return 2;
  if (zoom < 0.95) return 3;
  return 4;
}

/** A node is visible if its level is within `depth`, or its parent is visible and expanded. */
export function isVisible(n: MapNode, byId: Map<string, MapNode>, depth: number, expanded: Set<string>): boolean {
  if (levelIndex(n.level) < depth) return true;
  const p = n.parent ? byId.get(n.parent) : undefined;
  if (!p) return false;
  return expanded.has(p.id) && isVisible(p, byId, depth, expanded);
}

/** Tests and planted fixtures are hidden unless toggled on; proposals (ghosts) always show. */
export function shownOnMap(n: MapNode, showTests: boolean): boolean {
  if (n.ghost) return true;
  if (showTests) return true;
  return n.kind !== "test" && !(n.tags ?? []).includes("fixture");
}

export interface ElementsOptions {
  showTests: boolean;
  highlight?: Set<string> | null;
  selected?: string | null;
}

export function childCount(nodes: MapNode[]): Map<string, number> {
  const m = new Map<string, number>();
  for (const n of nodes) if (n.parent) m.set(n.parent, (m.get(n.parent) ?? 0) + 1);
  return m;
}

/** Cytoscape element definitions. Hidden levels are kept (display:none) so layouts stay stable. */
export function buildElements(data: MapData, opts: ElementsOptions) {
  const nodes = data.nodes.filter((n) => shownOnMap(n, opts.showTests));
  const ids = new Set(nodes.map((n) => n.id));
  const counts = childCount(nodes);
  const els: { group: "nodes" | "edges"; data: Record<string, unknown>; classes: string }[] = [];
  for (const n of nodes) {
    const parent = n.parent && ids.has(n.parent) ? n.parent : undefined;
    const classes = [
      n.level,
      `h-${n.health}`,
      n.kind ? `k-${n.kind}` : "",
      n.language ? `l-${n.language}` : "",
      n.ghost ? "ghost" : "",
      n.proposed_in?.length ? "proposed" : "",
      n.changed_since ? "changed" : "",
      opts.highlight ? (opts.highlight.has(n.id) ? "hl" : "dim") : "",
      opts.selected === n.id ? "sel" : "",
    ]
      .filter(Boolean)
      .join(" ");
    const kids = counts.get(n.id) ?? 0;
    els.push({
      group: "nodes",
      data: {
        id: n.id,
        parent,
        label: n.proposed_in?.length ? `Δ ${n.label}` : n.label,
        level: n.level,
        health: n.health,
        kind: n.kind ?? n.level,
        language: n.language ?? "",
        kids,
        sub: n.level === "symbol" ? `${n.kind ?? ""}${n.language ? " · " + (n.language === "rust" ? "rs" : "py") : ""}` : `${kids} inside`,
      },
      classes,
    });
  }
  for (const e of data.edges) {
    if (!ids.has(e.source) || !ids.has(e.target)) continue;
    if (e.kind === "member_of") continue; // shown by nesting/naming
    els.push({
      group: "edges",
      data: { id: e.id, source: e.source, target: e.target, kind: e.kind },
      classes: `e-${e.kind}` + (opts.highlight ? (opts.highlight.has(e.source) && opts.highlight.has(e.target) ? " hl" : " dim") : ""),
    });
  }
  return els;
}

export interface Pos { x: number; y: number }

export function nodeWidth(label: string): number {
  return Math.max(118, label.length * 7.9 + 40);
}

const ROW = 60;
const COL_GAP = 46;
const BOX_GAP = 90;

/**
 * Deterministic architecture layout (doc 05 map). Leaves get positions; compounds size
 * themselves around their children.
 * - inside a component: layered columns by dependency depth (callers/tests left, types right)
 * - components of a container side by side (callers first), containers stacked, contexts side by side
 */
export function computeLayout(data: MapData, showTests: boolean, aspect = 1.6): Map<string, Pos> {
  const nodes = data.nodes.filter((n) => shownOnMap(n, showTests));
  const ids = new Set(nodes.map((n) => n.id));
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const children = new Map<string, MapNode[]>();
  const roots: MapNode[] = [];
  for (const n of nodes) {
    if (n.parent && ids.has(n.parent)) {
      const arr = children.get(n.parent) ?? [];
      arr.push(n);
      children.set(n.parent, arr);
    } else roots.push(n);
  }
  const edges = data.edges.filter((e) => ids.has(e.source) && ids.has(e.target) && e.kind !== "tested_by");
  const out = new Map<string, Pos>();

  // size of a laid-out box, positions relative to its top-left
  type Box = { w: number; h: number; place: (x: number, y: number) => void };

  const leafBox = (leaves: MapNode[]): Box => {
    const set = new Set(leaves.map((l) => l.id));
    const rank = new Map<string, number>(leaves.map((l) => [l.id, l.kind === "test" ? 0 : 1]));
    const local = edges.filter((e) => set.has(e.source) && set.has(e.target) && e.source !== e.target);
    for (let i = 0; i < leaves.length + 1; i++) {
      let changed = false;
      for (const e of local) {
        const r = (rank.get(e.source) ?? 0) + 1;
        if (r > (rank.get(e.target) ?? 0) && r < leaves.length + 2) { rank.set(e.target, r); changed = true; }
      }
      if (!changed) break;
    }
    // types to the far right, ghosts in their own trailing column
    const maxR = Math.max(1, ...[...rank.values()]);
    for (const l of leaves) {
      if (["class", "enum", "trait"].includes(l.kind ?? "") && !l.ghost) rank.set(l.id, Math.max(rank.get(l.id)!, maxR));
      if (l.ghost) rank.set(l.id, maxR + 1);
    }
    const cols = new Map<number, MapNode[]>();
    for (const l of leaves) {
      const r = rank.get(l.id)!;
      cols.set(r, [...(cols.get(r) ?? []), l]);
    }
    const order = [...cols.keys()].sort((a, b) => a - b);
    const widths = order.map((r) => Math.max(...cols.get(r)!.map((l) => nodeWidth(l.label))));
    const w = widths.reduce((a, b) => a + b, 0) + COL_GAP * Math.max(0, order.length - 1);
    const h = Math.max(...order.map((r) => cols.get(r)!.length)) * ROW;
    return {
      w, h,
      place: (x0, y0) => {
        let x = x0;
        order.forEach((r, i) => {
          const col = cols.get(r)!.sort((a, b) => a.label.localeCompare(b.label));
          const colH = col.length * ROW;
          col.forEach((l, j) => out.set(l.id, { x: x + widths[i] / 2, y: y0 + (h - colH) / 2 + j * ROW + ROW / 2 }));
          x += widths[i] + COL_GAP;
        });
      },
    };
  };

  const PAD = 34;
  const boxFor = (n: MapNode): Box => {
    const kids = children.get(n.id) ?? [];
    if (kids.length === 0) {
      return { w: n.level === "symbol" ? nodeWidth(n.label) : 160, h: ROW, place: (x, y) => out.set(n.id, { x: x + (n.level === "symbol" ? nodeWidth(n.label) : 160) / 2, y: y + ROW / 2 }) };
    }
    const leaves = kids.filter((k) => k.level === "symbol");
    const compounds = kids.filter((k) => k.level !== "symbol");
    const parts: Box[] = [];
    // one band per language (Rust above Python) so each reads as its own call graph
    const langs = [...new Set(leaves.map((l) => l.language ?? ""))].sort((a, b) => (a === "rust" ? -1 : b === "rust" ? 1 : a.localeCompare(b)));
    for (const lang of langs) parts.push(leafBox(leaves.filter((l) => (l.language ?? "") === lang)));
    // order compounds: those with more outgoing cross edges first (callers left)
    const score = (c: MapNode) => {
      const inside = new Set<string>();
      const walk = (x: MapNode) => { inside.add(x.id); (children.get(x.id) ?? []).forEach(walk); };
      walk(c);
      let s = 0;
      for (const e of edges) {
        if (inside.has(e.source) && !inside.has(e.target)) s -= 1;
        if (inside.has(e.target) && !inside.has(e.source)) s += 1;
      }
      return s;
    };
    const sorted = [...compounds].sort((a, b) => score(a) - score(b) || a.label.localeCompare(b.label));
    const sub = sorted.map(boxFor);
    // contexts stack containers vertically; a container lays its components out across or down,
    // whichever fits the map's aspect ratio better (keeps labels legible at fit zoom)
    const fit = (w: number, h: number) => Math.min(aspect / (w / Math.max(h, 1)), (w / Math.max(h, 1)) / aspect);
    const across = { w: sub.reduce((a, b) => a + b.w, 0) + BOX_GAP * (sub.length - 1), h: Math.max(0, ...sub.map((b) => b.h)) };
    const down = { w: Math.max(0, ...sub.map((b) => b.w)), h: sub.reduce((a, b) => a + b.h, 0) + BOX_GAP * (sub.length - 1) };
    const horizontal = n.level !== "context" && (sub.length < 2 || fit(across.w, across.h) >= fit(down.w, down.h));
    if (sub.length) {
      const w = horizontal ? sub.reduce((a, b) => a + b.w, 0) + BOX_GAP * (sub.length - 1) : Math.max(...sub.map((b) => b.w));
      const h = horizontal ? Math.max(...sub.map((b) => b.h)) : sub.reduce((a, b) => a + b.h, 0) + BOX_GAP * (sub.length - 1);
      parts.push({
        w, h,
        place: (x, y) => {
          let cx = x, cy = y;
          sub.forEach((b) => {
            b.place(horizontal ? cx : x + (w - b.w) / 2, horizontal ? y + (h - b.h) / 2 : cy);
            if (horizontal) cx += b.w + BOX_GAP; else cy += b.h + BOX_GAP;
          });
        },
      });
    }
    const leafBands = langs.length - 1;
    const w = Math.max(...parts.map((p) => p.w)) + PAD * 2;
    const h = parts.reduce((a, p) => a + p.h, 0) + parts.map((_, i) => (i < leafBands ? ROW * 0.4 : BOX_GAP)).slice(0, -1).reduce((a, b) => a + b, 0) + PAD * 2;
    return {
      w, h,
      place: (x, y) => {
        let cy = y + PAD;
        parts.forEach((p, i) => {
          p.place(x + PAD + (w - PAD * 2 - p.w) / 2, cy);
          cy += p.h + (i < leafBands ? ROW * 0.4 : BOX_GAP);
        });
      },
    };
  };

  let x = 0;
  const rootBoxes = roots.sort((a, b) => levelIndex(a.level) - levelIndex(b.level) || a.label.localeCompare(b.label));
  const loose = rootBoxes.filter((r) => r.level === "symbol");
  for (const r of rootBoxes.filter((r) => r.level !== "symbol")) {
    const b = boxFor(r);
    b.place(x, 0);
    x += b.w + BOX_GAP * 1.5;
  }
  if (loose.length) {
    // symbols outside any component (orphans): their own block to the right
    const b = leafBox(loose);
    b.place(x, 0);
  }
  void byId;
  return out;
}

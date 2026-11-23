// Resizable panes: sizes the user dragged, persisted per viewer. Panes the user never touched are
// absent so the stylesheet's responsive defaults keep applying.
import type { CSSProperties } from "react";

export type Pane = "left" | "right" | "panel" | "term";
export type PaneSizes = Partial<Record<Pane, number>>;

export const STORAGE_KEY = "kodehub.layout.v1";
/** Narrowest the system map may get when a column is dragged wider. */
export const MIN_MAP = 320;

export const LIMITS: Record<Pane, { min: number; max: (viewport: number) => number }> = {
  left: { min: 240, max: () => 600 },
  right: { min: 200, max: () => 600 },
  panel: { min: 320, max: () => 800 },
  term: { min: 120, max: (vh) => Math.round(vh * 0.7) },
};

/** Clamp `size` to the pane's limits and to `room` (space left before another pane is squeezed). */
export function clampPane(pane: Pane, size: number, viewport: number, room = Infinity): number {
  const { min, max } = LIMITS[pane];
  const hi = Math.max(min, Math.min(max(viewport), room));
  return Math.round(Math.min(hi, Math.max(min, size)));
}

export function parseSizes(raw: string | null): PaneSizes {
  if (!raw) return {};
  try {
    const v = JSON.parse(raw);
    const out: PaneSizes = {};
    for (const p of Object.keys(LIMITS) as Pane[]) {
      const n = v?.[p];
      if (typeof n === "number" && Number.isFinite(n) && n > 0) out[p] = Math.round(n);
    }
    return out;
  } catch {
    return {};
  }
}

export function loadSizes(): PaneSizes {
  try { return parseSizes(localStorage.getItem(STORAGE_KEY)); } catch { return {}; }
}

export function saveSizes(s: PaneSizes) {
  try { localStorage.setItem(STORAGE_KEY, JSON.stringify(s)); } catch { /* storage unavailable */ }
}

/** Only the panes the user sized become inline custom properties. */
export function paneVars(s: PaneSizes, keys: Pane[]): CSSProperties {
  const names: Record<Pane, string> = { left: "--left-w", right: "--right-w", panel: "--panel-w", term: "--term-h" };
  const out: Record<string, string> = {};
  for (const k of keys) if (s[k] != null) out[names[k]] = `${s[k]}px`;
  return out as CSSProperties;
}

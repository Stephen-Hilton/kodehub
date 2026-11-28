// Health vocabulary shared by the map, badges and legends.
export const HEALTH: Record<string, { label: string; color: string; tint: string }> = {
  pass: { label: "Passing", color: "#34d399", tint: "rgba(52, 211, 153, 0.14)" },
  at_risk: { label: "At risk", color: "#fbbf24", tint: "rgba(251, 191, 36, 0.14)" },
  uncovered: { label: "Untested", color: "#fbbf24", tint: "rgba(251, 191, 36, 0.10)" },
  fail: { label: "Failing", color: "#f87171", tint: "rgba(248, 113, 113, 0.16)" },
  orphan: { label: "Orphan", color: "#64748b", tint: "rgba(100, 116, 139, 0.16)" },
  ghost: { label: "Proposed", color: "#a78bfa", tint: "rgba(167, 139, 250, 0.10)" },
  not_run: { label: "Not run", color: "#fbbf24", tint: "rgba(251, 191, 36, 0.10)" },
};

export function healthOf(status: string | undefined) {
  return HEALTH[status ?? "uncovered"] ?? HEALTH.uncovered;
}

/** Overall status for a set of statuses (worst wins). */
export function worst(statuses: string[]): string {
  const rank: Record<string, number> = { fail: 4, at_risk: 3, not_run: 3, uncovered: 2, orphan: 1, pass: 0 };
  let best = "pass";
  for (const s of statuses) if ((rank[s] ?? 0) > (rank[best] ?? 0)) best = s;
  return statuses.length ? best : "uncovered";
}

export const STATE_ORDER = ["draft", "planned", "approved", "building", "reviewing", "testing", "ready", "accepted"];

export function stateIndex(state: string): number {
  return STATE_ORDER.indexOf(state);
}

export function relTime(iso?: string | null, now = Date.now()): string {
  if (!iso) return "";
  const s = Math.round((now - new Date(iso).getTime()) / 1000);
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.round(h / 24)}d ago`;
}

import { describe, expect, it } from "vitest";
import type { MapData, MapNode } from "./api";
import { buildElements, depthForZoom, isVisible } from "./mapModel";

const n = (id: string, level: MapNode["level"], parent: string | null, extra: Partial<MapNode> = {}): MapNode => ({
  id, key: id, level, label: id, parent, health: "pass", ...extra,
});
const nodes = [
  n("ctx", "context", null),
  n("con", "container", "ctx"),
  n("cmp", "component", "con"),
  n("sym", "symbol", "cmp", { kind: "function", language: "rust" }),
  n("t", "symbol", "cmp", { kind: "test", language: "rust" }),
];
const byId = new Map(nodes.map((x) => [x.id, x]));

describe("semantic zoom", () => {
  it("maps zoom to depth", () => {
    expect(depthForZoom(0.2)).toBe(1);
    expect(depthForZoom(0.6)).toBe(2);
    expect(depthForZoom(0.8)).toBe(3);
    expect(depthForZoom(1.5)).toBe(4);
  });
  it("shows levels up to depth, and children of expanded nodes", () => {
    const vis = (id: string, d: number, ex: string[] = []) => isVisible(byId.get(id)!, byId, d, new Set(ex));
    expect(vis("ctx", 1)).toBe(true);
    expect(vis("con", 1)).toBe(false);
    expect(vis("con", 1, ["ctx"])).toBe(true);
    expect(vis("sym", 3)).toBe(false);
    expect(vis("sym", 3, ["cmp"])).toBe(true);
    expect(vis("sym", 4)).toBe(true);
  });
  it("builds elements with health, ghost and highlight classes", () => {
    const data: MapData = {
      mission: null, at: null,
      nodes: [...nodes, n("g", "symbol", "cmp", { ghost: true, health: "ghost", kind: "test" })],
      edges: [{ id: "e1", source: "t", target: "sym", kind: "calls" }, { id: "e2", source: "sym", target: "cmp", kind: "member_of" }],
    };
    const els = buildElements(data, { showTests: true, highlight: new Set(["sym", "t"]) });
    const sym = els.find((e) => e.data.id === "sym")!;
    expect(sym.classes).toContain("h-pass");
    expect(sym.classes).toContain("hl");
    expect(els.find((e) => e.data.id === "ctx")!.classes).toContain("dim");
    expect(els.find((e) => e.data.id === "g")!.classes).toContain("ghost");
    expect(els.filter((e) => e.group === "edges")).toHaveLength(1);
    const noTests = buildElements(data, { showTests: false });
    expect(noTests.find((e) => e.data.id === "t")).toBeUndefined();
  });
});

import { computeLayout } from "./mapModel";
describe("architecture layout", () => {
  it("places callers left of callees and every leaf exactly once", () => {
    const data: MapData = {
      mission: null, at: null,
      nodes: [
        n("ctx", "context", null), n("con", "container", "ctx"), n("cmp", "component", "con"),
        n("caller", "symbol", "cmp", { kind: "function" }), n("callee", "symbol", "cmp", { kind: "function" }),
        n("T", "symbol", "cmp", { kind: "class" }), n("test", "symbol", "cmp", { kind: "test" }),
        n("lonely", "symbol", null, { kind: "function" }),
      ],
      edges: [
        { id: "1", source: "caller", target: "callee", kind: "calls" },
        { id: "2", source: "test", target: "caller", kind: "calls" },
        { id: "3", source: "callee", target: "T", kind: "depends_on" },
      ],
    };
    const p = computeLayout(data, true);
    expect(p.get("test")!.x).toBeLessThan(p.get("caller")!.x);
    expect(p.get("caller")!.x).toBeLessThan(p.get("callee")!.x);
    expect(p.get("callee")!.x).toBeLessThan(p.get("T")!.x);
    expect(p.has("lonely")).toBe(true);
    expect(p.has("cmp")).toBe(false); // compounds size themselves
    expect(computeLayout(data, false).has("test")).toBe(false);
  });
});

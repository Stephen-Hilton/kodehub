import { describe, expect, it } from "vitest";
import { clampPane, paneVars, parseSizes } from "./layout";

describe("pane layout", () => {
  it("clamps to per-pane limits", () => {
    expect(clampPane("left", 50, 900)).toBe(240);
    expect(clampPane("left", 9999, 900)).toBe(600);
    expect(clampPane("panel", 500.4, 900)).toBe(500);
    expect(clampPane("term", 2000, 1000)).toBe(700);
    expect(clampPane("term", 10, 1000)).toBe(120);
  });

  it("never takes more than the room left for the map, but keeps the minimum", () => {
    expect(clampPane("right", 550, 900, 400)).toBe(400);
    expect(clampPane("right", 550, 900, 100)).toBe(200);
  });

  it("parses stored sizes defensively", () => {
    expect(parseSizes(null)).toEqual({});
    expect(parseSizes("not json")).toEqual({});
    expect(parseSizes('{"left":300.6,"right":"x","panel":-4,"term":250,"bogus":1}')).toEqual({ left: 301, term: 250 });
  });

  it("emits custom properties only for sized panes", () => {
    expect(paneVars({ left: 300, term: 200 }, ["left", "right", "panel"])).toEqual({ "--left-w": "300px" });
    expect(paneVars({ term: 200 }, ["term"])).toEqual({ "--term-h": "200px" });
  });
});

import { describe, expect, it } from "vitest";
import { relTime, stateIndex, worst } from "./health";

describe("health helpers", () => {
  it("worst status wins", () => {
    expect(worst(["pass", "at_risk"])).toBe("at_risk");
    expect(worst(["pass", "fail", "at_risk"])).toBe("fail");
    expect(worst([])).toBe("uncovered");
  });
  it("orders changeset states", () => {
    expect(stateIndex("planned")).toBeLessThan(stateIndex("merged"));
  });
  it("formats relative time", () => {
    const now = Date.parse("2026-01-01T00:10:00Z");
    expect(relTime("2026-01-01T00:09:58Z", now)).toBe("just now");
    expect(relTime("2026-01-01T00:05:00Z", now)).toBe("5m ago");
  });
});

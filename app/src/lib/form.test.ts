import { describe, expect, it } from "vitest";
import { coerce, defaultArgs, defaultFor, isRunnableSpec, renameKey, setIn, type Spec } from "./form";

const user: Spec = {
  type: "struct",
  name: "User",
  fields: [
    { name: "username", raw: "String", spec: { type: "string" } },
    { name: "locked", raw: "bool", spec: { type: "bool" } },
  ],
};
const store: Spec = { type: "struct", name: "UserStore", fields: [{ name: "users", raw: "HashMap", spec: { type: "map", value: user } }] };

describe("run-now form model", () => {
  it("builds defaults for nested specs", () => {
    expect(defaultFor(store)).toEqual({ users: {} });
    expect(defaultArgs([{ name: "store", spec: store }, { name: "n", spec: { type: "int" } }])).toEqual({ store: { users: {} }, n: 0 });
    expect(defaultFor({ type: "enum", name: "E", variants: ["a", "b"] })).toBe("a");
  });
  it("sets nested values immutably", () => {
    const a = { store: { users: {} } };
    const b = setIn(a, ["store", "users", "alice"], defaultFor(user));
    expect(b).toEqual({ store: { users: { alice: { username: "", locked: false } } } });
    expect(a).toEqual({ store: { users: {} } });
    expect(setIn([1, 2], [1], 5)).toEqual([1, 5]);
  });
  it("coerces primitives and renames keys in order", () => {
    expect(coerce({ type: "int" }, "42")).toBe(42);
    expect(coerce({ type: "float" }, "x")).toBe(0);
    expect(renameKey({ a: 1, b: 2 }, "a", "z")).toEqual({ z: 1, b: 2 });
    expect(Object.keys(renameKey({ a: 1, b: 2 }, "a", "z"))).toEqual(["z", "b"]);
  });
  it("detects unsupported nested types", () => {
    expect(isRunnableSpec(store)).toBe(true);
    expect(isRunnableSpec({ type: "list", inner: { type: "unsupported", reason: "fn" } })).toBe(false);
  });
});

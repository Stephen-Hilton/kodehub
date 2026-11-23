// Run-now form model: build default values from a server form spec and turn form state into
// the JSON args the harness expects. Pure functions (unit tested).
import type { Json } from "./api";

export type Spec =
  | { type: "int" | "float" | "bool" | "string" | "json" }
  | { type: "option"; inner: Spec }
  | { type: "list"; inner: Spec }
  | { type: "map"; value: Spec }
  | { type: "struct"; name: string; fields: { name: string; raw: string; spec: Spec }[] }
  | { type: "enum"; name: string; variants: string[] }
  | { type: "unsupported"; reason: string };

export function defaultFor(spec: Spec): Json {
  switch (spec.type) {
    case "int":
    case "float":
      return 0;
    case "bool":
      return false;
    case "string":
      return "";
    case "option":
      return null;
    case "list":
      return [];
    case "map":
      return {};
    case "struct": {
      const o: Record<string, Json> = {};
      for (const f of spec.fields) o[f.name] = defaultFor(f.spec);
      return o;
    }
    case "enum":
      return spec.variants[0] ?? "";
    default:
      return null;
  }
}

export function defaultArgs(params: { name: string; spec: Spec }[]): Record<string, Json> {
  const out: Record<string, Json> = {};
  for (const p of params) out[p.name] = defaultFor(p.spec);
  return out;
}

/** Coerce a raw input string for a primitive spec. */
export function coerce(spec: Spec, raw: string): Json {
  switch (spec.type) {
    case "int": {
      const n = parseInt(raw, 10);
      return Number.isNaN(n) ? 0 : n;
    }
    case "float": {
      const n = parseFloat(raw);
      return Number.isNaN(n) ? 0 : n;
    }
    case "bool":
      return raw === "true";
    default:
      return raw;
  }
}

/** Set a value at a path inside nested objects/arrays, returning a new root. */
export function setIn(root: Json, path: (string | number)[], value: Json): Json {
  if (path.length === 0) return value;
  const [head, ...rest] = path;
  const base = Array.isArray(root) ? [...root] : { ...(root ?? {}) };
  (base as Json)[head] = setIn(root == null ? undefined : root[head as Json], rest, value);
  return base;
}

/** Rename a map key preserving order. */
export function renameKey(obj: Record<string, Json>, from: string, to: string): Record<string, Json> {
  const out: Record<string, Json> = {};
  for (const [k, v] of Object.entries(obj)) out[k === from ? to : k] = v;
  return out;
}

export function isRunnableSpec(spec: Spec): boolean {
  if (spec.type === "unsupported") return false;
  if (spec.type === "option" || spec.type === "list") return isRunnableSpec(spec.inner);
  if (spec.type === "map") return isRunnableSpec(spec.value);
  if (spec.type === "struct") return spec.fields.every((f) => isRunnableSpec(f.spec));
  return true;
}

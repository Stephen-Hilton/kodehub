import { useState } from "react";
import type { Json } from "../lib/api";
import { coerce, defaultFor, renameKey, setIn, type Spec } from "../lib/form";
import { Plus, X } from "./icons";

function typeLabel(spec: Spec): string {
  switch (spec.type) {
    case "option": return `${typeLabel(spec.inner)}?`;
    case "list": return `[${typeLabel(spec.inner)}]`;
    case "map": return `{str: ${typeLabel(spec.value)}}`;
    case "struct": return spec.name;
    case "enum": return spec.name;
    default: return spec.type;
  }
}

/** One field of a generated form, recursive over nested types. */
export function Field({ spec, value, onChange, path, testid }: {
  spec: Spec; value: Json; onChange: (path: (string | number)[], v: Json) => void; path: (string | number)[]; testid: string;
}) {
  switch (spec.type) {
    case "string": {
      // paste the last run result (e.g. a hash from hash_password) straight into the field
      const last = (window as Json).__kodehubLastResult as string | undefined;
      return (
        <div className="field-wrap">
          <input className="input mono" value={value ?? ""} onChange={(e) => onChange(path, e.target.value)} data-testid={testid}
            style={last ? { paddingRight: 92 } : undefined} />
          {last && (
            <button className="btn sm ghost-btn paste-last" title={`Paste: ${last.slice(0, 60)}`} data-testid={`${testid}-paste`}
              onClick={() => onChange(path, last)}>⇣ last result</button>
          )}
        </div>
      );
    }
    case "int":
    case "float":
      return <input className="input mono" type="number" step={spec.type === "float" ? "any" : 1} value={value ?? 0}
        onChange={(e) => onChange(path, coerce(spec, e.target.value))} data-testid={testid} />;
    case "bool":
      return (
        <label className="row" style={{ cursor: "pointer", height: 30 }}>
          <input type="checkbox" checked={!!value} onChange={(e) => onChange(path, e.target.checked)} data-testid={testid} />
          <span className="muted">{value ? "true" : "false"}</span>
        </label>
      );
    case "enum":
      return (
        <select className="select" value={value ?? ""} onChange={(e) => onChange(path, e.target.value)} data-testid={testid}>
          {spec.variants.map((v) => <option key={v} value={v}>{v}</option>)}
        </select>
      );
    case "option":
      return (
        <div>
          <label className="row" style={{ cursor: "pointer", marginBottom: 4 }}>
            <input type="checkbox" checked={value !== null && value !== undefined}
              onChange={(e) => onChange(path, e.target.checked ? defaultFor(spec.inner) : null)} />
            <span className="muted" style={{ fontSize: 12 }}>{value === null || value === undefined ? "None" : "Some"}</span>
          </label>
          {value !== null && value !== undefined && <Field spec={spec.inner} value={value} onChange={onChange} path={path} testid={testid} />}
        </div>
      );
    case "list": {
      const items: Json[] = Array.isArray(value) ? value : [];
      return (
        <div className="nested">
          {items.map((it, i) => (
            <div className="row" key={i} style={{ marginBottom: 6, alignItems: "flex-start" }}>
              <div style={{ flex: 1 }}><Field spec={spec.inner} value={it} onChange={onChange} path={[...path, i]} testid={`${testid}-${i}`} /></div>
              <button className="btn icon ghost-btn" aria-label="Remove" onClick={() => onChange(path, items.filter((_, j) => j !== i))}><X size={12} /></button>
            </div>
          ))}
          <button className="btn sm ghost-btn" onClick={() => onChange(path, [...items, defaultFor(spec.inner)])}><Plus size={12} /> Add item</button>
        </div>
      );
    }
    case "map": {
      const obj: Record<string, Json> = value && typeof value === "object" ? value : {};
      return (
        <div className="nested">
          {Object.entries(obj).map(([k, v], i) => (
            <div className="map-entry" key={i}>
              <div className="row" style={{ marginBottom: 6 }}>
                <input className="input mono" style={{ width: 160 }} value={k} placeholder="key" data-testid={`${testid}-key-${i}`}
                  onChange={(e) => onChange(path, renameKey(obj, k, e.target.value))} />
                <span className="spacer" />
                <button className="btn icon ghost-btn" aria-label="Remove entry" onClick={() => { const o = { ...obj }; delete o[k]; onChange(path, o); }}><X size={12} /></button>
              </div>
              <Field spec={spec.value} value={v} onChange={onChange} path={[...path, k]} testid={`${testid}-${i}`} />
            </div>
          ))}
          <button className="btn sm ghost-btn" data-testid={`${testid}-add`}
            onClick={() => { let n = 1; while (`key${n}` in obj) n++; onChange(path, { ...obj, [`key${n}`]: defaultFor(spec.value) }); }}>
            <Plus size={12} /> Add entry
          </button>
        </div>
      );
    }
    case "struct":
      return (
        <div className="nested">
          {spec.fields.map((f) => (
            <div className="form-field" key={f.name}>
              <label className="label">{f.name} <span className="type">{f.raw}</span></label>
              <Field spec={f.spec} value={value?.[f.name]} onChange={onChange} path={[...path, f.name]} testid={`${testid}-${f.name}`} />
            </div>
          ))}
        </div>
      );
    default:
      return <div className="faint">unsupported</div>;
  }
}

export function RunForm({ params, args, setArgs }: {
  params: { name: string; raw: string; spec: Spec; default?: string }[]; args: Record<string, Json>; setArgs: (a: Record<string, Json>) => void;
}) {
  const [json, setJson] = useState(false);
  const [text, setText] = useState("");
  const [err, setErr] = useState("");
  const onChange = (path: (string | number)[], v: Json) => setArgs(setIn(args, path, v));
  return (
    <div>
      <div className="row" style={{ marginBottom: 8 }}>
        <span className="faint" style={{ fontSize: 11.5 }}>{params.length === 0 ? "No parameters" : "Arguments"}</span>
        <span className="spacer" />
        <div className="seg" style={{ background: "var(--s1)", borderRadius: 7 }}>
          <button className={!json ? "on" : ""} onClick={() => { if (json) { try { setArgs(JSON.parse(text || "{}")); setErr(""); setJson(false); } catch (e) { setErr(String(e)); } } }}>Form</button>
          <button className={json ? "on" : ""} onClick={() => { setText(JSON.stringify(args, null, 2)); setJson(true); }} data-testid="args-json-toggle">JSON</button>
        </div>
      </div>
      {json ? (
        <>
          <textarea className="textarea code" value={text} data-testid="args-json"
            onChange={(e) => { setText(e.target.value); try { setArgs(JSON.parse(e.target.value || "{}")); setErr(""); } catch (x) { setErr(String(x)); } }} />
          {err && <div style={{ color: "var(--fail)", fontSize: 11.5 }}>{err}</div>}
        </>
      ) : (
        params.map((p) => (
          <div className="form-field" key={p.name}>
            <label className="label">{p.name} <span className="type">{p.raw || typeLabel(p.spec)}</span></label>
            <Field spec={p.spec} value={args[p.name]} onChange={onChange} path={[p.name]} testid={`arg-${p.name}`} />
          </div>
        ))
      )}
    </div>
  );
}

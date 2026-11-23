import { useCallback, useEffect, useRef, useState } from "react";
import { api, type Json, type Requirement } from "../lib/api";
import { defaultArgs } from "../lib/form";
import { healthOf, relTime } from "../lib/health";
import { onKodehubEvent } from "../lib/useKodehub";
import { Beaker, Branch, Check, Clock, Diamond, Edit, Link, Play, Save, Scale, X } from "./icons";
import { StatusBadge } from "./RequirementsPanel";
import { RunForm } from "./RunForm";
import { useToast } from "./Toasts";

const argsKey = (k: string) => `kodehub.args.${k}`;
const loadArgs = (k: string): Record<string, Json> | null => {
  try { const v = localStorage.getItem(argsKey(k)); return v ? JSON.parse(v) : null; } catch { return null; }
};
const saveArgs = (k: string, a: Record<string, Json>) => { try { localStorage.setItem(argsKey(k), JSON.stringify(a)); } catch { /* ignore */ } };

export function NodePanel({ symbolKey, environment, onClose, onNavigate, onOpenRequirement, requirements }: {
  symbolKey: string; environment: string; onClose: () => void; onNavigate: (id: string) => void; onOpenRequirement: (key: string) => void;
  requirements: Requirement[];
}) {
  const toast = useToast();
  const [d, setD] = useState<Json>(null);
  const [args, setArgs] = useState<Record<string, Json>>({});
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<Json>(null);
  const [stream, setStream] = useState<string[]>([]);
  const [saveOpen, setSaveOpen] = useState(false);
  const [exReq, setExReq] = useState("");
  const [exDesc, setExDesc] = useState("");
  const [editing, setEditing] = useState(false);
  const [body, setBody] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const token = useRef("");
  // args are initialised once per symbol; a late (or repeated) load never clobbers typed input
  const initFor = useRef<string | null>(null);

  const load = useCallback(() => {
    api.symbol(symbolKey).then((x) => {
      setD(x);
      setBody(x.version?.body ?? "");
    }).catch((e) => toast("error", String(e.message)));
  }, [symbolKey, toast]);

  useEffect(() => {
    setD(null); setResult(null); setStream([]); setEditing(false); setSaveOpen(false);
    api.symbol(symbolKey).then((x) => {
      setD(x);
      setBody(x.version?.body ?? "");
      // remembered args for this symbol, else defaults
      if (initFor.current !== symbolKey) {
        initFor.current = symbolKey;
        setArgs({ ...defaultArgs(x.form?.params ?? []), ...(loadArgs(symbolKey) ?? {}) });
      }
      setExReq(x.requirements?.[0]?.id?.split("/")[1] ?? "");
    }).catch((e) => toast("error", String(e.message)));
  }, [symbolKey, toast]);

  useEffect(() => onKodehubEvent((e) => {
    if (e.type === "run.output" && e.data?.token === token.current && e.data.line !== undefined) {
      setStream((s) => [...s.slice(-200), `${e.data.stream === "stderr" ? "! " : ""}${e.data.line}`]);
    }
    if (e.type === "node.changed" || e.type === "test.result") load();
  }), [load]);

  const run = async () => {
    saveArgs(symbolKey, args);
    setRunning(true); setResult(null); setStream([]); setSaveOpen(false);
    token.current = Math.random().toString(36).slice(2);
    try {
      const r = await api.run(symbolKey, args, environment, token.current);
      if (r.ok && r.result !== null && r.result !== undefined) {
        (window as Json).__kodehubLastResult = typeof r.result === "string" ? r.result : JSON.stringify(r.result);
      }
      setResult(r);
    } catch (e) {
      setResult({ ok: false, stderr: String((e as Error).message), result: null });
    } finally {
      setRunning(false);
    }
  };

  const saveExample = async () => {
    try {
      await api.addExample(exReq, { description: exDesc || `${d.symbol.display_name} example`, input: args, expected: result.result, run: result._key });
      toast("success", "Saved as example — the tester agent will turn it into a test.");
      setSaveOpen(false);
    } catch (e) {
      toast("error", String((e as Error).message));
    }
  };

  // Manual text edit → a changeset that still goes through blind review and tests.
  const submitEdit = async () => {
    setSubmitting(true);
    try {
      await api.manualEdit(symbolKey, body);
      toast("success", "Changeset opened — the blind reviewer and every affected test run before it can merge.");
      setEditing(false);
    } catch (e) {
      toast("error", String((e as Error).message));
    } finally {
      setSubmitting(false);
    }
  };

  // examples / edge cases of the requirements this symbol serves, usable as run-now inputs
  const served = new Set((d?.requirements ?? []).map((r: Json) => String(r.id)));
  const paramNames = new Set<string>((d?.form?.params ?? []).map((p: Json) => p.name));
  const exampleOptions = requirements
    .filter((r) => served.has(r._id))
    .flatMap((r) => [...(r.examples ?? []).map((e) => ({ r, e, kind: "example" })), ...(r.edge_cases ?? []).map((e) => ({ r, e, kind: "edge case" }))])
    .filter(({ e }) => e.input && typeof e.input === "object" && Object.keys(e.input).some((k) => paramNames.has(k)));
  const applyExample = (idx: number) => {
    const o = exampleOptions[idx];
    if (!o) return;
    const next = { ...args };
    for (const [k, v] of Object.entries(o.e.input as Record<string, Json>)) if (paramNames.has(k)) next[k] = v;
    setArgs(next);
    setExDesc(o.e.description);
    toast("info", `Filled from ${o.kind}: “${o.e.description}” — expected ${JSON.stringify(o.e.expected)}`);
  };

  if (!d) {
    return (
      <div className="node-panel" data-testid="node-panel">
        <div className="np-head"><div className="skeleton" style={{ width: "60%", height: 18 }} /><div className="skeleton" style={{ width: "85%", marginTop: 10 }} /></div>
        <div className="np-body">{[1, 2, 3].map((i) => <div key={i} className="skeleton" style={{ height: 60, marginTop: 16 }} />)}</div>
      </div>
    );
  }
  const s = d.symbol;
  const h = healthOf(d.health);
  return (
    <div className="node-panel" data-testid="node-panel">
      <div className="np-head">
        <div className="row" style={{ alignItems: "flex-start" }}>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div className="row" style={{ marginBottom: 6, gap: 6 }}>
              <span className="chip">{s.kind}</span>
              <span className="chip">{s.language}</span>
              {s.tags?.map((t: string) => <span className="chip" key={t}>{t}</span>)}
              <span className={`badge ${d.health}`} data-testid="np-health">{h.label}</span>
            </div>
            <div className="np-name" data-testid="np-name">{s.display_name}</div>
            <div className="np-sig">{d.version.signature || "(no signature)"}{d.form.returns ? "" : ""}</div>
          </div>
          <button className="btn icon ghost-btn" aria-label="Close panel" onClick={onClose}><X size={15} /></button>
        </div>
        {s.description && <div className="muted" style={{ marginTop: 8, fontSize: 12.5 }}>{s.description}</div>}
      </div>
      <div className="np-body">
        {/* run now */}
        <div className="np-section">
          <div className="section-title"><span>Run now</span><span className="chip">env: {environment}</span></div>
          {d.form.runnable ? (
            <div className="run-box">
              {exampleOptions.length > 0 && (
                <div className="row" style={{ marginBottom: 10 }}>
                  <select className="select" value="" onChange={(e) => applyExample(Number(e.target.value))} data-testid="example-picker">
                    <option value="" disabled>Fill from a requirement example…</option>
                    {exampleOptions.map((o, i) => <option key={i} value={i}>{o.r.title.split("—")[0].trim()} · {o.kind}: {o.e.description}</option>)}
                  </select>
                </div>
              )}
              <RunForm params={d.form.params} args={args} setArgs={setArgs} />
              <div className="row" style={{ marginTop: 10 }}>
                <button className="btn primary" onClick={run} disabled={running} data-testid="run-now">
                  {running ? <span className="spin" /> : <Play size={12} />} Run {s.display_name}
                </button>
                <span className="faint" style={{ fontSize: 11.5 }}>10 s limit · no network</span>
              </div>
              {(result || stream.length > 0) && (
                <div className={`output ${result ? (result.ok ? "ok" : "bad") : ""}`} data-testid="run-output">
                  <div className="output-head">
                    {result ? (result.ok ? <Check size={13} /> : <X size={13} />) : <span className="spin" />}
                    <span>{result ? (result.ok ? "Returned" : result.timed_out ? "Timed out" : "Failed") : "Running…"}</span>
                    <span className="spacer" />
                    {result && (
                      <span data-testid="run-meta">
                        {result.cache_hit ? "⚡ cache hit" : "compiled"} · {result.duration_ms} ms{result.release ? ` · ${result.release}` : ""}
                      </span>
                    )}
                  </div>
                  {result && result.result !== null && result.result !== undefined && (
                    <div className="output-result" data-testid="run-result">
                      <button className="btn sm ghost-btn copy" title="Copy result" data-testid="copy-result"
                        onClick={() => { const v = typeof result.result === "string" ? result.result : JSON.stringify(result.result); navigator.clipboard?.writeText(v).catch(() => {}); (window as Json).__kodehubLastResult = v; toast("info", "Result copied"); }}>
                        Copy
                      </button>
                      {JSON.stringify(result.result, null, 2)}
                    </div>
                  )}
                  {(stream.length > 0 || result?.stderr) && (
                    <div className="output-stream">{stream.join("\n")}{result?.stderr && !stream.length ? result.stderr : ""}</div>
                  )}
                </div>
              )}
              {result?.ok && d.requirements?.length > 0 && (
                <div style={{ marginTop: 10 }}>
                  {!saveOpen ? (
                    <button className="btn sm" onClick={() => setSaveOpen(true)} data-testid="save-example"><Save size={12} /> Save as example</button>
                  ) : (
                    <div className="card" style={{ padding: 10 }}>
                      <label className="label" style={{ marginTop: 0 }}>Requirement this example belongs to</label>
                      <select className="select" value={exReq} onChange={(e) => setExReq(e.target.value)} data-testid="example-req">
                        {d.requirements.map((r: Json) => <option key={r.id} value={r.id.split("/")[1]}>{r.title}</option>)}
                      </select>
                      <label className="label">Description</label>
                      <input className="input" value={exDesc} onChange={(e) => setExDesc(e.target.value)} placeholder="e.g. Locked user with the right password" data-testid="example-desc" />
                      <div className="faint" style={{ fontSize: 11.5, marginTop: 6 }}>expected = <span className="mono">{JSON.stringify(result.result)}</span></div>
                      <div className="row" style={{ marginTop: 8 }}>
                        <span className="spacer" />
                        <button className="btn sm" onClick={() => setSaveOpen(false)}>Cancel</button>
                        <button className="btn sm primary" onClick={saveExample} data-testid="example-save">Save example</button>
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>
          ) : (
            <div className="run-box faint" style={{ fontSize: 12.5 }}>Run now is unavailable: {d.form.reason}</div>
          )}
        </div>

        <div className="np-section">
          <div className="section-title"><span>Requirements served</span></div>
          <div className="link-list">
            {[...(d.groups ?? []), ...(d.requirements ?? [])].map((r: Json) => (
              <div className="link-item" key={r.id} onClick={() => r.id.startsWith("requirements/") && onOpenRequirement(r.id.split("/")[1])}>
                <Diamond size={12} /> <span style={{ flex: 1 }}>{r.title}</span>
                <span className="faint" style={{ fontSize: 11 }}>{r.id.startsWith("requirement_groups") ? "group" : "requirement"}</span>
              </div>
            ))}
            {!(d.requirements?.length || d.groups?.length) && <div className="faint">No requirement reaches this symbol — it is an orphan.</div>}
          </div>
        </div>

        <div className="np-section">
          <div className="section-title"><span>Dependencies</span><span className="faint">{d.dependencies.length} out · {d.dependents.length} in</span></div>
          <div className="link-list">
            {d.dependencies.map((x: Json) => (
              <div className="link-item" key={"o" + x.id} onClick={() => onNavigate(`symbols/${x.id}`)}>
                <Link size={12} /> <span className="mono" style={{ flex: 1 }}>{x.display_name}</span><span className="faint">{x.edge}</span>
              </div>
            ))}
            {d.dependents.map((x: Json) => (
              <div className="link-item" key={"i" + x.id} onClick={() => onNavigate(`symbols/${x.id}`)}>
                <Branch size={12} /> <span className="mono" style={{ flex: 1 }}>{x.display_name}</span><span className="faint">used by · {x.edge}</span>
              </div>
            ))}
          </div>
        </div>

        <div className="np-section">
          <div className="section-title"><span>Tests</span><span className="faint">{d.tests.filter((t: Json) => t.status === "pass").length}/{d.tests.length} passing</span></div>
          <div className="link-list">
            {d.tests.map((t: Json) => (
              <div className="link-item" key={t.id} onClick={() => onNavigate(`symbols/${t.id}`)} title={t.output ?? ""}>
                <Beaker size={12} /> <span className="mono" style={{ flex: 1 }}>{t.name}</span><StatusBadge status={t.status} />
              </div>
            ))}
            {d.tests.length === 0 && <div className="faint">No tests reach this symbol.</div>}
          </div>
        </div>

        {d.decisions?.length > 0 && (
          <div className="np-section">
            <div className="section-title"><span>Decisions</span></div>
            {d.decisions.map((x: Json) => (
              <div className="card" key={x._key} style={{ padding: 10, marginBottom: 6 }} data-testid="decision">
                <div className="row"><Scale size={13} /><b style={{ flex: 1 }}>{x.title}</b><span className="faint" style={{ fontSize: 11 }}>{x.made_by}</span></div>
                <div style={{ marginTop: 4, fontSize: 12 }}>{x.choice}</div>
                <div className="faint" style={{ marginTop: 4, fontSize: 11.5 }}>{x.rationale}</div>
              </div>
            ))}
          </div>
        )}

        <div className="np-section">
          <div className="section-title"><span>Version history</span><span className="faint">closure {String(d.closure_hash).slice(0, 10)}</span></div>
          {d.history.map((v: Json, i: number) => (
            <div className={`history-item ${i > 0 ? "old" : ""}`} key={v._key + i}>
              <span className="tl" />
              <div>
                <span className="mono">{v._key.slice(0, 12)}</span> {v.current && <span className="badge pass" style={{ marginLeft: 4 }}>current</span>}
                <div className="faint" style={{ fontSize: 11.5 }}>by {v.created_by}</div>
              </div>
              <span className="faint" style={{ fontSize: 11.5 }}><Clock size={11} /> {relTime(v.since ?? v.created_at)}</span>
            </div>
          ))}
        </div>

        <div className="np-section">
          <div className="section-title">
            <span>Text view</span>
            {!editing ? (
              <button className="btn sm ghost-btn" onClick={() => setEditing(true)} data-testid="edit-text"><Edit size={12} /> Edit</button>
            ) : (
              <span className="row">
                <button className="btn sm" onClick={() => { setEditing(false); setBody(d.version.body); }}>Cancel</button>
                <button className="btn sm primary" disabled={submitting || body === d.version.body} onClick={submitEdit} data-testid="submit-edit">
                  {submitting ? <span className="spin" /> : null} Submit for review
                </button>
              </span>
            )}
          </div>
          {editing ? (
            <textarea className="textarea code" value={body} onChange={(e) => setBody(e.target.value)} spellCheck={false} data-testid="text-editor" />
          ) : (
            <pre className="code-block">{d.version.body}</pre>
          )}
          {editing && <div className="faint" style={{ fontSize: 11.5, marginTop: 6 }}>Saving opens a changeset: the blind reviewer and every affected test run before it can merge.</div>}
        </div>
      </div>
    </div>
  );
}

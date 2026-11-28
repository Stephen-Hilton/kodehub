import { useEffect, useMemo, useRef, useState } from "react";
import { api, type Example, type Json, type Requirement, type TagCoverage } from "../lib/api";
import { relTime } from "../lib/health";
import { Clock, Plus, Save, X } from "./icons";
import { StatusBadge } from "./RequirementsPanel";
import { useToast } from "./Toasts";

/** Stable stringify (sorted keys) so key order never reads as an edit. */
const canon = (v: Json): string =>
  v === null || typeof v !== "object" ? JSON.stringify(v ?? null)
    : Array.isArray(v) ? `[${v.map(canon).join(",")}]`
    : `{${Object.keys(v).sort().map((k) => `${JSON.stringify(k)}:${canon(v[k])}`).join(",")}}`;
const show = (v: Json) => (v === null || v === undefined ? "" : typeof v === "string" ? v : JSON.stringify(v));
const parse = (s: string): Json => {
  const t = s.trim();
  if (!t) return null;
  try { return JSON.parse(t); } catch { return t; }
};

function StringList({ items, onChange, placeholder }: { items: string[]; onChange: (v: string[]) => void; placeholder: string }) {
  return (
    <div className="list-edit">
      {items.map((it, i) => (
        <div className="list-item" key={i}>
          <input className="input" value={it} placeholder={placeholder} onChange={(e) => onChange(items.map((x, j) => (j === i ? e.target.value : x)))} />
          <button className="btn icon ghost-btn" aria-label="Remove" onClick={() => onChange(items.filter((_, j) => j !== i))}><X size={13} /></button>
        </div>
      ))}
      <button className="btn sm ghost-btn" style={{ alignSelf: "flex-start" }} onClick={() => onChange([...items, ""])}><Plus size={12} /> Add</button>
    </div>
  );
}

interface Ex { description: string; input: string; expected: string; isNew?: boolean }
const toEx = (e: Example): Ex => ({ description: e.description, input: show(e.input), expected: show(e.expected) });
const fromEx = (e: Ex): Example => ({ description: e.description.trim(), input: parse(e.input), expected: parse(e.expected) });

function ExampleList({ items, onChange, testid }: { items: Ex[]; onChange: (v: Ex[]) => void; testid: string }) {
  return (
    <div className="list-edit" data-testid={testid}>
      {items.map((e, i) => {
        const set = (patch: Partial<Ex>) => onChange(items.map((x, j) => (j === i ? { ...x, ...patch } : x)));
        return (
          <div className={`example ${e.isNew ? "new" : ""}`} key={i}>
            <div className="row">
              <input className="input" placeholder="Description (e.g. A locked account must also return invalid_credentials)"
                value={e.description} onChange={(ev) => set({ description: ev.target.value })} autoFocus={e.isNew}
                data-testid={`${testid}-desc-${i}`} />
              <button className="btn icon ghost-btn" aria-label="Remove" onClick={() => onChange(items.filter((_, j) => j !== i))}><X size={13} /></button>
            </div>
            <div className="example-io">
              <textarea className="textarea" placeholder='input — JSON (e.g. {"username": "alice"}) or text' rows={2} value={e.input} onChange={(ev) => set({ input: ev.target.value })} data-testid={`${testid}-input-${i}`} />
              <textarea className="textarea" placeholder="expected" rows={2} value={e.expected} onChange={(ev) => set({ expected: ev.target.value })} data-testid={`${testid}-expected-${i}`} />
            </div>
          </div>
        );
      })}
    </div>
  );
}

/** Editable requirement anatomy (doc 05). `focusEdgeCase` opens with a fresh edge-case row. */
export function RequirementEditor({ reqKey, tags, focusEdgeCase, onClose, onCreated }: {
  reqKey: string | null; tags: TagCoverage[]; focusEdgeCase: boolean; onClose: () => void; onCreated?: (key: string) => void;
}) {
  const toast = useToast();
  const creating = reqKey === null;
  const [orig, setOrig] = useState<Requirement | null>(null);
  const [title, setTitle] = useState("");
  const [statement, setStatement] = useState("");
  const [intent, setIntent] = useState("");
  const [constraints, setConstraints] = useState<string[]>([]);
  const [examples, setExamples] = useState<Ex[]>([]);
  const [edgeCases, setEdgeCases] = useState<Ex[]>([]);
  const [outOfScope, setOutOfScope] = useState<string[]>([]);
  const [tagText, setTagText] = useState("");
  const [history, setHistory] = useState<Json[]>([]);
  const [saving, setSaving] = useState(false);
  const [tab, setTab] = useState<"anatomy" | "history">("anatomy");
  // every version but the current one (requirement versions are immutable; proposed and
  // rejected changes show with their state)
  const earlier = history.filter((h) => !(h.revision === orig?.revision && h.state === "accepted"));
  const edgeRef = useRef<HTMLDivElement>(null);
  // fields are initialised once per requirement: a late/repeated load never clobbers edits
  const initFor = useRef<string | null>(null);

  useEffect(() => {
    if (creating) return;
    api.requirement(reqKey!).then((r) => {
      if (initFor.current === reqKey) return;
      initFor.current = reqKey;
      setOrig(r);
      setTitle(r.title ?? ""); setStatement(r.statement ?? ""); setIntent(r.intent ?? "");
      setConstraints(r.constraints ?? []); setExamples((r.examples ?? []).map(toEx));
      const ec = (r.edge_cases ?? []).map(toEx);
      setEdgeCases(focusEdgeCase ? [...ec, { description: "", input: "", expected: "", isNew: true }] : ec);
      setOutOfScope(r.out_of_scope ?? []);
      if (focusEdgeCase) window.setTimeout(() => {
        // scroll only the drawer body (scrollIntoView would also scroll the page)
        const el = edgeRef.current;
        const body = el?.closest(".drawer-body") as HTMLElement | null;
        if (el && body) body.scrollTo({ top: el.offsetTop - body.clientHeight / 3, behavior: "smooth" });
      }, 60);
    });
    api.requirementHistory(reqKey!).then(setHistory).catch(() => {});
  }, [reqKey, creating, focusEdgeCase]);

  const patch = useMemo(() => {
    if (!orig) return {};
    const p: Record<string, Json> = {};
    const cmp = (k: string, v: Json, o: Json) => { if (canon(v) !== canon(o ?? (Array.isArray(v) ? [] : ""))) p[k] = v; };
    cmp("title", title, orig.title); cmp("statement", statement, orig.statement); cmp("intent", intent, orig.intent);
    cmp("constraints", constraints.filter((c) => c.trim()), orig.constraints);
    cmp("examples", examples.filter((e) => e.description.trim()).map(fromEx), orig.examples);
    cmp("edge_cases", edgeCases.filter((e) => e.description.trim()).map(fromEx), orig.edge_cases);
    cmp("out_of_scope", outOfScope.filter((c) => c.trim()), orig.out_of_scope);
    return p;
  }, [orig, title, statement, intent, constraints, examples, edgeCases, outOfScope]);
  const dirty = Object.keys(patch).length > 0;

  const save = async () => {
    setSaving(true);
    try {
      if (creating) {
        const r = await api.createRequirement({ title, statement, intent, constraints, out_of_scope: outOfScope,
          examples: examples.map(fromEx), edge_cases: edgeCases.map(fromEx),
          tags: tagText.split(",").map((t) => t.trim()).filter(Boolean) });
        toast("success", `Created “${title || statement}”`);
        onCreated?.(r._key);
        onClose();
      } else {
        await api.updateRequirement(reqKey!, patch);
        toast("success", `${title} updated — everything beneath it is now at risk and the orchestrator is planning the change.`);
        onClose();
      }
    } catch (e) {
      toast("error", String((e as Error).message));
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <div className="drawer-backdrop light" onClick={onClose} />
      <div className="drawer" role="dialog" aria-label="Requirement" data-testid="requirement-editor">
        <div className="drawer-head">
          <div style={{ flex: 1 }}>
            <div className="faint" style={{ fontSize: 11, letterSpacing: "0.1em", textTransform: "uppercase" }}>
              {creating ? "New requirement" : `Requirement · rev ${orig?.revision ?? ""}`}
            </div>
            <div className="drawer-title">{creating ? "Describe the requirement" : title}</div>
          </div>
          {orig && <StatusBadge status={orig.status} />}
          <button className="btn icon ghost-btn" aria-label="Close" onClick={onClose}><X size={15} /></button>
        </div>
        {!creating && (
          <div className="tabs">
            <button className={`tab ${tab === "anatomy" ? "active" : ""}`} onClick={() => setTab("anatomy")}>Anatomy</button>
            <button className={`tab ${tab === "history" ? "active" : ""}`} onClick={() => setTab("history")}>
              <Clock size={12} /> History <span className="count">{earlier.length}</span>
            </button>
          </div>
        )}
        <div className="drawer-body anatomy">
          {!creating && !orig ? (
            <div style={{ marginTop: 16 }}>{[1, 2, 3, 4].map((i) => <div key={i} className="skeleton" style={{ height: 34, marginTop: 12 }} />)}</div>
          ) : tab === "anatomy" ? (
            <>
              <label className="label">Title</label>
              <input className="input" value={title} onChange={(e) => setTitle(e.target.value)} placeholder="R3 — Short name" data-testid="req-title" />
              <label className="label">Statement</label>
              <textarea className="textarea" value={statement} onChange={(e) => setStatement(e.target.value)} placeholder="What must be true" data-testid="req-statement" />
              <label className="label">Intent</label>
              <textarea className="textarea" value={intent} onChange={(e) => setIntent(e.target.value)} placeholder="Why it exists" />
              {creating && (
                <>
                  <label className="label">Tags</label>
                  <input className="input" list="req-tags" value={tagText} onChange={(e) => setTagText(e.target.value)}
                    placeholder="comma separated, e.g. secure-authentication" data-testid="req-tags" />
                  <datalist id="req-tags">{tags.map((g) => <option key={g.tag} value={g.tag} />)}</datalist>
                </>
              )}
              <h4>Constraints</h4>
              <StringList items={constraints} onChange={setConstraints} placeholder="A rule or limit" />
              <h4>Examples</h4>
              <ExampleList items={examples} onChange={setExamples} testid="examples" />
              <button className="btn sm ghost-btn" style={{ marginTop: 6 }} onClick={() => setExamples([...examples, { description: "", input: "", expected: "", isNew: true }])}>
                <Plus size={12} /> Add example
              </button>
              <div ref={edgeRef}>
                <h4>Edge cases <span className="badge at_risk" style={{ textTransform: "none", letterSpacing: 0 }}>how most bugs get fixed</span></h4>
                <ExampleList items={edgeCases} onChange={setEdgeCases} testid="edge-cases" />
                <button className="btn sm" style={{ marginTop: 6 }} data-testid="editor-add-edge"
                  onClick={() => setEdgeCases([...edgeCases, { description: "", input: "", expected: "", isNew: true }])}>
                  <Plus size={12} /> Add edge case
                </button>
              </div>
              <h4>Out of scope</h4>
              <StringList items={outOfScope} onChange={setOutOfScope} placeholder="Deliberately not covered" />
              {orig && (
                <>
                  <h4>Verified by</h4>
                  <div className="link-list">
                    {orig.tests.map((t) => (
                      <div className="link-item" key={t.test}>
                        <span className="dot" style={{ background: t.status === "pass" ? "var(--pass)" : t.status === "fail" ? "var(--fail)" : "var(--risk)" }} />
                        <span className="mono" style={{ flex: 1 }}>{t.name}</span>
                        <span className="faint">{t.language}</span>
                      </div>
                    ))}
                    {orig.tests.length === 0 && <div className="faint">No tests yet.</div>}
                  </div>
                </>
              )}
            </>
          ) : (
            <div style={{ marginTop: 12 }}>
              {earlier.length === 0 && <div className="empty">No earlier revisions.</div>}
              {earlier.map((h) => (
                <div key={h._key} className="card" style={{ padding: 12, marginBottom: 8 }}>
                  <div className="row"><b>Revision {h.revision}</b>{h.state !== "accepted" && <span className="badge ghost">{h.state}</span>}<span className="spacer" /><span className="faint">{relTime(h.created_at)} · by {h.created_by}</span></div>
                  <div className="muted" style={{ marginTop: 6 }}>{h.snapshot?.statement}</div>
                  <div className="faint" style={{ marginTop: 4, fontSize: 12 }}>{h.snapshot?.edge_cases?.length ?? 0} edge cases · {h.snapshot?.examples?.length ?? 0} examples</div>
                </div>
              ))}
            </div>
          )}
        </div>
        <div className="drawer-foot">
          {!creating && <span className="faint" style={{ marginRight: "auto", alignSelf: "center", fontSize: 12 }}>
            {dirty ? `Changed: ${Object.keys(patch).join(", ")}` : "No changes"}
          </span>}
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn primary" disabled={saving || (creating ? !statement.trim() : !dirty)} onClick={save} data-testid="req-save">
            {saving ? <span className="spin" /> : <Save size={13} />} {creating ? "Create" : "Save revision"}
          </button>
        </div>
      </div>
    </>
  );
}

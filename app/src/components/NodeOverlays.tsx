// Overlays opened from in-node badges (spec §8.3): next to the element, closable, never a side
// panel. Tests (runs, history, Test now), requirements (own + inherited, with the cascade),
// lifecycle (state, versions) and the work on the node.
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { api, localKey, type Json, type MapNode } from "../lib/api";
import { relTime } from "../lib/health";
import { X } from "./icons";
import { LIFECYCLE_LABEL, type BadgeKind } from "./NodeBadges";
import { useToast } from "./Toasts";

/** A floating card anchored at a viewport point, kept on screen. */
export function Popover({ x, y, onClose, children, width = 380, testid }: {
  x: number; y: number; onClose: () => void; children: React.ReactNode; width?: number; testid?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const left = Math.max(8, Math.min(x, window.innerWidth - r.width - 8));
    const top = y + r.height > window.innerHeight - 8 ? Math.max(8, y - r.height - 24) : y;
    setPos({ left, top });
  }, [x, y]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="popover glass" ref={ref} style={{ left: pos.left, top: pos.top, width }} role="dialog" data-testid={testid}>
      <button className="btn icon ghost-btn popover-close" aria-label="Close" onClick={onClose}><X size={13} /></button>
      {children}
    </div>
  );
}

const STATUS_CLASS: Record<string, string> = { pass: "pass", fail: "fail", not_run: "", error: "fail" };

function TestsBody({ node }: { node: MapNode }) {
  const toast = useToast();
  const [tests, setTests] = useState<Json[] | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => { api.nodeTests(node.id).then(setTests).catch(() => setTests([])); }, [node.id]);
  const t = node.test_counts;
  return (
    <>
      <div className="pop-head">
        <b>Tests · {node.label}</b>
        <span className="faint">{t ? `${t.total.passed}/${t.total.total} pass${node.level !== "symbol" ? ` · ${t.own.total} directly here` : ""}` : ""}</span>
      </div>
      <div className="pop-body">
        {tests === null && <div className="faint">Loading…</div>}
        {tests?.length === 0 && <div className="faint">No tests cover this node yet.</div>}
        {tests?.map((x) => (
          <div key={x.key} className="pop-row">
            <button className="pop-row-main" onClick={() => setOpen(open === x.key ? null : x.key)}>
              <span className={`badge ${STATUS_CLASS[x.status] ?? ""}`}>{x.status === "not_run" ? "not run" : x.status}</span>
              <span className="mono">{x.name}</span>
              <span className="faint">{x.runs?.[0]?.ran_at ? relTime(x.runs[0].ran_at) : ""}</span>
            </button>
            {open === x.key && (
              <div className="pop-history">
                {(x.runs ?? []).map((r: Json, i: number) => (
                  <div key={i} className="faint">
                    <span className={`dot ${r.status === "pass" ? "ok" : "bad"}`} /> {r.status} · {r.formal ? "formal" : "informal"}{r.current ? "" : " · older version"} · {relTime(r.ran_at)}
                    {r.status !== "pass" && r.output && <pre className="code-block">{r.output}</pre>}
                  </div>
                ))}
              </div>
            )}
          </div>
        ))}
      </div>
      <div className="pop-foot">
        <button className="btn sm primary" disabled={busy} data-testid="test-now" onClick={async () => {
          setBusy(true);
          try { await api.testNow(node.level === "symbol" ? node.id : node.id); toast("success", `Test run queued for ${node.label}`); }
          catch (e) { toast("error", (e as Error).message); } finally { setBusy(false); }
        }}>Test now</button>
        <span className="faint">Runs in test containers; no LLM is involved.</span>
      </div>
    </>
  );
}

function RequirementsBody({ node, onOpenRequirement }: { node: MapNode; onOpenRequirement: (k: string) => void }) {
  const [c, setC] = useState<Json>(null);
  useEffect(() => { api.cascade(node.id).then(setC).catch(() => setC({ requirements: [], chain: [] })); }, [node.id]);
  const rq = node.requirements;
  const levels: string[] = c?.chain ?? [];
  return (
    <>
      <div className="pop-head">
        <b>Requirements · {node.label}</b>
        {rq && <span className="faint">business {rq.own.business}·{rq.total.business} · technical {rq.own.technical}·{rq.total.technical}</span>}
      </div>
      <div className="pop-body" data-testid="cascade">
        {c === null && <div className="faint">Loading…</div>}
        {c && c.requirements.length === 0 && <div className="faint">No requirements apply here yet.</div>}
        {levels.map((lvl: string) => {
          const here = (c?.requirements ?? []).filter((r: Json) => r.attached_to === lvl);
          if (!here.length) return null;
          return (
            <div key={lvl} className="cascade-level">
              <div className="cascade-label">{lvl === node.id ? "On this node" : lvl.startsWith("projects/") ? "Inherited from the project" : `Inherited from ${lvl}`}</div>
              {here.map((r: Json) => (
                <button key={r.id} className="pop-row-main" onClick={() => onOpenRequirement(r.key)}>
                  <span className={`chip kind-${r.kind}`}>{r.kind === "business" ? "B" : r.kind === "technical" ? "T" : r.kind}</span>
                  <span>{r.title ?? r.statement}</span>
                </button>
              ))}
            </div>
          );
        })}
        {c?.philosophy && <div className="cascade-philosophy faint">Tiebreaker: {c.philosophy}</div>}
      </div>
    </>
  );
}

function LifecycleBody({ node }: { node: MapNode }) {
  const [versions, setVersions] = useState<Json[] | null>(null);
  const [work, setWork] = useState<Json[]>([]);
  useEffect(() => {
    api.nodeVersions(node.id).then(setVersions).catch(() => setVersions([]));
    api.workItems().then((w) => setWork(w.filter((x: Json) => x.target === node.id))).catch(() => {});
  }, [node.id]);
  const life = node.lifecycle ?? "planned";
  const explain: Record<string, string> = {
    planned: "Exists in the architecture; nothing is built yet.",
    in_build: "Work is active, a candidate is pending, or it is only partly built.",
    ready_for_test: "Built, and its tests exist; formal tests have not all passed.",
    live: "A version passed formal tests and was promoted (for parents: everything beneath is live).",
  };
  return (
    <>
      <div className="pop-head"><b>{LIFECYCLE_LABEL[life]} · {node.label}</b></div>
      <div className="pop-body">
        <p className="faint" style={{ marginTop: 0 }}>{explain[life]}</p>
        <div className="cascade-label">Versions</div>
        {versions?.length === 0 && <div className="faint">No versions yet.</div>}
        {versions?.slice(0, 8).map((v) => (
          <div key={`${v.seq}-${v.version}`} className="pop-row-main static">
            <span className={`badge ${v.current ? "pass" : v.state === "candidate" ? "risk" : ""}`}>{v.current ? "current" : v.state === "accepted" ? "earlier" : v.state}</span>
            <span className="mono">v{v.seq ?? "?"} · {localKey(String(v.version)).slice(0, 10)}</span>
            <span className="faint">{v.project_versions?.length ? v.project_versions.map((p: Json) => p.name).join(", ") : relTime(v.created_at)}</span>
          </div>
        ))}
        {work.length > 0 && <div className="cascade-label">Work on this node</div>}
        {work.map((w) => <WorkRow key={w._key} w={w} />)}
      </div>
    </>
  );
}

export function WorkRow({ w }: { w: Json }) {
  const tone = w.state === "done" ? "pass" : w.state === "failed" || w.state === "blocked" ? "fail" : w.state === "claimed" ? "risk" : "";
  return (
    <div className="pop-row-main static" data-testid={`work-${w._key}`}>
      <span className={`badge ${tone}`}>{w.state}</span>
      <span>{w.agent_type} · {w.kind || "work"}</span>
      {w.priority >= 100 && <span className="chip">high</span>}
      <span className="faint">{relTime(w.updated_at ?? w.created_at)}</span>
    </div>
  );
}

function WorkBody({ node }: { node: MapNode }) {
  const [work, setWork] = useState<Json[] | null>(null);
  useEffect(() => { api.workItems().then((w) => setWork(w.filter((x: Json) => x.target === node.id))).catch(() => setWork([])); }, [node.id]);
  return (
    <>
      <div className="pop-head"><b>Work in progress · {node.label}</b></div>
      <div className="pop-body">
        {work === null && <div className="faint">Loading…</div>}
        {work?.map((w) => <WorkRow key={w._key} w={w} />)}
      </div>
    </>
  );
}

export function NodeOverlay({ node, kind, x, y, onClose, onOpenRequirement }: {
  node: MapNode; kind: BadgeKind; x: number; y: number; onClose: () => void; onOpenRequirement: (k: string) => void;
}) {
  return (
    <Popover x={x} y={y} onClose={onClose} testid={`overlay-${kind}`}>
      {kind === "tests" && <TestsBody node={node} />}
      {kind === "requirements" && <RequirementsBody node={node} onOpenRequirement={onOpenRequirement} />}
      {kind === "lifecycle" && <LifecycleBody node={node} />}
      {kind === "work" && <WorkBody node={node} />}
    </Popover>
  );
}

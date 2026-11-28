// Philosophy and requirements (spec §7, §8.4): edit the philosophy document (the tiebreaker),
// browse and edit requirements by kind and level, see where each attaches and what inherits it,
// and act on push-down suggestions.
import { useEffect, useState } from "react";
import { Insights } from "../components/Insights";
import { RequirementsPanel } from "../components/RequirementsPanel";
import { useToast } from "../components/Toasts";
import { api, localKey, type Json, type Requirement, type TagCoverage } from "../lib/api";

export function RequirementsScreen({ requirements, tags, onOpen, onAddEdgeCase, onNew, onRunTests, running, onSelectNode, selected, setSelected }: {
  requirements: Requirement[]; tags: TagCoverage[]; onOpen: (k: string) => void; onAddEdgeCase: (k: string) => void; onNew: () => void;
  onRunTests: () => void; running: boolean; onSelectNode: (id: string) => void;
  /** The selected requirement (its path is lit on the map). */
  selected: string | null; setSelected: (k: string | null) => void;
}) {
  const toast = useToast();
  const [ph, setPh] = useState<Json>(null);
  const [text, setText] = useState("");
  const [by, setBy] = useState<Json[]>([]);
  const [kind, setKind] = useState("");
  const [push, setPush] = useState<Json[]>([]);
  useEffect(() => {
    api.philosophy().then((p) => { setPh(p); setText(p?.text ?? ""); }).catch(() => {});
    api.pushDown().then(setPush).catch(() => {});
  }, []);
  useEffect(() => { api.requirementsBy(kind ? { kind } : {}).then(setBy).catch(() => {}); }, [kind, requirements]);
  const attached = (k: string) => by.find((r) => r.key === k);
  return (
    <div className="screen req-screen" data-testid="requirements-screen">
      <div className="card philosophy">
        <div className="section-head">
          <span className="section-title">Philosophy document</span>
          <span className="faint">the vision and strategic goals — the tiebreaker when requirements are ambiguous or conflict</span>
          <span className="spacer" />
          {ph?.revision && <span className="faint">revision {ph.revision}</span>}
        </div>
        <textarea className="textarea" rows={3} value={text} onChange={(e) => setText(e.target.value)} data-testid="philosophy-text" />
        <div className="row">
          <span className="spacer" />
          <button className="btn sm primary" disabled={!text.trim() || text === ph?.text} data-testid="philosophy-save"
            onClick={() => api.setPhilosophy(text).then((p) => { setPh(p); toast("success", "Philosophy saved"); }).catch((e) => toast("error", e.message))}>Save</button>
        </div>
      </div>
      <div className="req-grid">
        <div className="req-list-col">
          <RequirementsPanel requirements={requirements} tags={tags} selected={selected} onSelect={setSelected}
            onOpen={onOpen} onAddEdgeCase={onAddEdgeCase} onNew={onNew} onRunTests={onRunTests} running={running}
            insights={<Insights onSelect={onSelectNode} />} />
        </div>
        <div className="card cascade-col">
          <div className="section-head">
            <span className="section-title">Where requirements attach</span>
            <span className="spacer" />
            <div className="seg">{["", "business", "technical"].map((k) => <button key={k} className={kind === k ? "on" : ""} onClick={() => setKind(k)}>{k || "all"}</button>)}</div>
          </div>
          <div className="faint" style={{ fontSize: 12, marginBottom: 8 }}>Each level inherits everything above it. Put a requirement at the lowest level where it applies.</div>
          {by.map((r) => (
            <button key={r.key} className={`pop-row-main ${selected === r.key ? "on" : ""}`} onClick={() => setSelected(r.key)} data-testid={`attach-${localKey(r.key)}`}>
              <span className={`chip kind-${r.kind}`}>{r.kind === "business" ? "B" : "T"}</span>
              <span>{r.title}</span>
              <span className="spacer" />
              <span className="faint">{(r.levels ?? []).join(", ") || "not attached"}</span>
            </button>
          ))}
          {selected && attached(selected) && (
            <div className="cascade-level">
              <div className="cascade-label">Attached to</div>
              {(attached(selected)!.attached_to ?? []).map((n: string) => (
                <button key={n} className="pop-row-main" onClick={() => onSelectNode(n)}><span className="mono">{n}</span></button>
              ))}
            </div>
          )}
          {push.length > 0 && (
            <div className="cascade-level" data-testid="push-down">
              <div className="cascade-label">Push-down suggestions</div>
              {push.map((p, i) => (
                <div key={i} className="faint">{p.requirement} on {p.attached_to} only reaches {p.suggest} — consider moving it there.</div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

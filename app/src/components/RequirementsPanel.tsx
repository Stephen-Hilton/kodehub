import { useMemo } from "react";
import { localKey, type Requirement, type TagCoverage } from "../lib/api";
import { healthOf } from "../lib/health";
import { Beaker, Edit, Play, Plus } from "./icons";

export function StatusBadge({ status }: { status: string }) {
  return <span className={`badge ${status}`}>{healthOf(status).label}</span>;
}

export function RequirementsPanel({ requirements, tags, selected, onSelect, onOpen, onAddEdgeCase, onNew, onRunTests, running, insights }: {
  requirements: Requirement[]; tags: TagCoverage[]; selected: string | null;
  onSelect: (key: string | null) => void; onOpen: (key: string) => void; onAddEdgeCase: (key: string) => void;
  onNew: () => void; onRunTests: () => void; running: boolean; insights?: React.ReactNode;
}) {
  const byId = useMemo(() => new Map(requirements.map((r) => [r._id, r])), [requirements]);
  // requirements group by tag (one may carry several tags and show under each)
  const grouped = new Set(tags.flatMap((g) => g.members));
  const ungrouped = requirements.filter((r) => !grouped.has(r._id));
  const counts = { pass: 0, at_risk: 0, fail: 0, uncovered: 0 } as Record<string, number>;
  for (const r of requirements) counts[r.status === "uncovered" ? "uncovered" : r.status] = (counts[r.status] ?? 0) + 1;
  const total = requirements.length || 1;

  const item = (r: Requirement) => {
    const h = healthOf(r.status);
    const passed = r.tests.filter((t) => t.status === "pass").length;
    return (
      <div
        key={r._key}
        className={`req ${selected === r._key ? "active" : ""}`}
        data-testid={`req-${localKey(r._key)}`}
        onClick={() => onSelect(selected === r._key ? null : r._key)}
        onDoubleClick={() => onOpen(r._key)}
      >
        <div className="req-bar" style={{ background: h.color }} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <div className="row">
            <div className="req-title" style={{ flex: 1 }}>{r.title}</div>
            <StatusBadge status={r.status} />
          </div>
          <div className="req-statement">{r.statement}</div>
          <div className="req-meta">
            <span className="chip" title="Tests behind this requirement"><Beaker size={11} /> {passed}/{r.tests.length}</span>
            <span className="chip" title="Edge cases">{r.edge_cases.length} edge case{r.edge_cases.length === 1 ? "" : "s"}</span>
            <span className="chip" title="Revision">rev {r.revision}</span>
            <span className="spacer" />
            <button className="btn sm ghost-btn" title="Add edge case" data-testid={`add-edge-${localKey(r._key)}`}
              onClick={(e) => { e.stopPropagation(); onAddEdgeCase(r._key); }}>
              <Plus size={12} /> Edge case
            </button>
            <button className="btn sm ghost-btn icon" title="Open requirement" aria-label="Open requirement"
              onClick={(e) => { e.stopPropagation(); onOpen(r._key); }}>
              <Edit size={12} />
            </button>
          </div>
        </div>
      </div>
    );
  };

  return (
    <aside className="col sidebar" data-testid="requirements-panel">
      <div className="section-head">
        <span className="section-title">Requirements</span>
        <div className="row">
          <button className="btn sm" onClick={onRunTests} disabled={running} title="Run every test (cached passes are reused)" data-testid="run-all-tests">
            {running ? <span className="spin" /> : <Play size={11} />} Tests
          </button>
          <button className="btn sm icon" onClick={onNew} title="New requirement" aria-label="New requirement"><Plus size={13} /></button>
        </div>
      </div>
      <div className="coverage-bar" title="Coverage">
        {(["pass", "at_risk", "uncovered", "fail"] as const).map((k) =>
          counts[k] ? <div key={k} style={{ width: `${(counts[k] / total) * 100}%`, background: healthOf(k).color }} /> : null,
        )}
      </div>
      <div className="legend">
        <span><i className="dot" style={{ background: "var(--pass)" }} />{counts.pass} passing</span>
        <span><i className="dot" style={{ background: "var(--risk)" }} />{(counts.at_risk ?? 0) + (counts.uncovered ?? 0)} at risk</span>
        <span><i className="dot" style={{ background: "var(--fail)" }} />{counts.fail ?? 0} failing</span>
      </div>
      <div className="scroll">
        {tags.map((g) => (
          <div className="group" key={g.tag} data-testid={`tag-${g.tag}`}>
            <div className="group-head">
              <span className="dot" style={{ background: healthOf(g.status).color }} />
              <span style={{ flex: 1 }}>#{g.tag}</span>
              <span className="group-logic">{g.members.length}</span>
              <StatusBadge status={g.status} />
            </div>
            {g.members.map((m) => byId.get(m)).filter(Boolean).map((r) => item(r!))}
          </div>
        ))}
        {ungrouped.length > 0 && (
          <div className="group">
            <div className="group-head"><span className="faint" style={{ flex: 1 }}>Untagged</span></div>
            {ungrouped.map(item)}
          </div>
        )}
        {requirements.length === 0 && <div className="empty">No requirements yet. Seed the project or create one.</div>}
        {requirements.length > 0 && insights}
      </div>
    </aside>
  );
}

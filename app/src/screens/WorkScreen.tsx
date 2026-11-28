// Work queue and agent activity (spec §8.4): what needs a human (approvals, external blockers),
// the queue, spend, and the agents' live activity and proposals.
import { useEffect, useState } from "react";
import { AgentsPanel } from "../components/AgentsPanel";
import { WorkRow } from "../components/NodeOverlays";
import { useToast } from "../components/Toasts";
import { api, type Json, type Proposal } from "../lib/api";

export function WorkScreen({ agents, proposals, reqTitle }: { agents: Json; proposals: Proposal[]; reqTitle: (k: string) => string }) {
  const toast = useToast();
  const [items, setItems] = useState<Json[]>([]);
  const [stats, setStats] = useState<Json>(null);
  const [approvals, setApprovals] = useState<Json[]>([]);
  const [notes, setNotes] = useState<Json[]>([]);
  const [filter, setFilter] = useState<string>("open");
  const load = () => {
    api.workItems().then(setItems).catch(() => {});
    api.queueStats().then(setStats).catch(() => {});
    api.approvals().then(setApprovals).catch(() => {});
    api.notifications().then(setNotes).catch(() => {});
  };
  useEffect(() => { load(); const t = window.setInterval(load, 4000); return () => window.clearInterval(t); }, []); // eslint-disable-line react-hooks/exhaustive-deps
  const shown = items.filter((w) => filter === "all" || (filter === "open" ? ["queued", "claimed", "waiting_human", "blocked"].includes(w.state) : w.state === filter));
  const count = (st: string) => (stats?.counts ?? []).filter((c: Json) => c.state === st).reduce((a: number, c: Json) => a + c.n, 0);
  return (
    <div className="screen work-screen" data-testid="work-screen">
      <div className="screen-head">
        <h2>Work</h2>
        <span className="chip">{count("queued")} queued</span>
        <span className="chip">{count("claimed")} running</span>
        <span className="chip">{count("blocked")} blocked</span>
        <span className="chip">${(stats?.spend?.cost_usd ?? 0).toFixed(2)} spent{stats?.at_budget ? " · at budget" : ""}</span>
      </div>
      {notes.length > 0 && (
        <div className="card attention" data-testid="blockers">
          <b>Needs you — external blockers (not code problems)</b>
          {notes.map((n) => (
            <div key={n._key} className="row">
              <span className="badge fail">blocked</span><span>{n.message}</span><span className="spacer" />
              <button className="btn sm" onClick={() => api.readNotification(n._key).then(load)}>Dismiss</button>
            </div>
          ))}
          <div className="row"><span className="spacer" />
            <button className="btn sm primary" onClick={() => api.unblock().then((r) => { toast("success", `${r.requeued} item(s) back in the queue`); load(); })}>I fixed it — retry blocked work</button>
          </div>
        </div>
      )}
      {approvals.length > 0 && (
        <div className="card attention" data-testid="approvals">
          <b>Needs your approval</b>
          {approvals.map((a) => (
            <div key={a._key} className="approval">
              <span className="chip">{a.kind.replace("_", " ")}</span>
              <span>{a.proposal?.requirement_doc?.title ?? a.proposal?.name ?? a.proposal?.requirement ?? "proposal"}</span>
              <span className="faint">{a.rationale}</span>
              <span className="spacer" />
              <button className="btn sm" onClick={() => api.decide(a._key, false).then(load).catch((e) => toast("error", e.message))}>Reject</button>
              <button className="btn sm primary" data-testid={`approve-${a._key}`} onClick={() => api.decide(a._key, true).then(load).catch((e) => toast("error", e.message))}>Approve</button>
            </div>
          ))}
        </div>
      )}
      <div className="work-grid">
        <div className="card">
          <div className="section-head">
            <span className="section-title">Queue</span>
            <span className="spacer" />
            <div className="seg">{["open", "done", "failed", "all"].map((f) => <button key={f} className={filter === f ? "on" : ""} onClick={() => setFilter(f)}>{f}</button>)}</div>
          </div>
          <div className="queue">
            {shown.length === 0 && <div className="empty">Nothing here.</div>}
            {shown.map((w) => (
              <div key={w._key} className="queue-row">
                <WorkRow w={w} />
                <span className="faint mono">{w.target}</span>
                {["queued", "blocked"].includes(w.state) && <button className="btn sm ghost-btn" onClick={() => api.cancelWork(w._key).then(load)}>Cancel</button>}
              </div>
            ))}
          </div>
        </div>
        <div className="activity-col">
          <AgentsPanel agents={agents} proposals={proposals} requirementsTitle={reqTitle} selectedNode={null} onClearRef={() => {}} />
        </div>
      </div>
    </div>
  );
}

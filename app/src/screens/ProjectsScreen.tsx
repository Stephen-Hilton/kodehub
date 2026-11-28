// Project list and the create-project flow (spec §6.1): name and description, sharing, the
// philosophy document (typed or spoken), starting contexts, and a template.
import { useEffect, useState } from "react";
import { api, session, type Json } from "../lib/api";
import { relTime } from "../lib/health";
import { Recorder } from "../components/Recorder";
import { useToast } from "../components/Toasts";

export function ProjectsScreen({ current, onOpen }: { current: string | null; onOpen: (id: string) => void }) {
  const toast = useToast();
  const [projects, setProjects] = useState<Json[] | null>(null);
  const [creating, setCreating] = useState(false);
  const load = () => api.projects().then(setProjects).catch((e) => toast("error", e.message));
  useEffect(() => { load(); }, []); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <div className="screen" data-testid="projects-screen">
      <div className="screen-head">
        <h2>Projects</h2>
        <span className="spacer" />
        <button className="btn primary" onClick={() => setCreating(true)} data-testid="new-project">New project</button>
      </div>
      {creating && <CreateProject projects={projects ?? []} onCancel={() => setCreating(false)} onCreated={(id) => { setCreating(false); load(); onOpen(id); }} />}
      <div className="card list">
        {projects?.length === 0 && <div className="empty">No projects yet — create one.</div>}
        {projects?.map((p) => (
          <button key={p.id} className={`list-item ${p.id === current ? "on" : ""}`} onClick={() => onOpen(p.id)} data-testid={`project-${p.id}`}>
            <div>
              <div style={{ fontWeight: 600 }}>{p.name}</div>
              <div className="faint" style={{ fontSize: 12 }}>
                {p.description || "—"} · {p.counts?.code ?? 0} code nodes · {p.counts?.tests ?? 0} tests · {p.counts?.requirements ?? 0} requirements
                {p.last_opened_at ? ` · opened ${relTime(p.last_opened_at)}` : ""}
              </div>
            </div>
            <span className="spacer" />
            <span className="chip">{String(p.owner ?? "").replace("users/", "").replace("orgs/", "org: ")}</span>
            <span className={`badge ${p.permission === "admin" ? "pass" : ""}`}>{p.permission}</span>
          </button>
        ))}
      </div>
    </div>
  );
}

function CreateProject({ projects, onCancel, onCreated }: { projects: Json[]; onCancel: () => void; onCreated: (id: string) => void }) {
  const toast = useToast();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [sharing, setSharing] = useState<"private" | "org" | "users">("private");
  const [org, setOrg] = useState("");
  const [orgs, setOrgs] = useState<Json[]>([]);
  const [users, setUsers] = useState<Json[]>([]);
  const [shareUsers, setShareUsers] = useState<string[]>([]);
  const [philosophy, setPhilosophy] = useState("");
  const [transcript, setTranscript] = useState<string | null>(null);
  const [common, setCommon] = useState<{ name: string; description: string }[]>([]);
  const [contexts, setContexts] = useState<string[]>([]);
  const [custom, setCustom] = useState("");
  const [template, setTemplate] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    api.commonContexts().then(setCommon).catch(() => {});
    api.orgs().then((o) => { setOrgs(o); if (o[0]) setOrg(o[0]._key); }).catch(() => {});
    api.users().then(setUsers).catch(() => {});
  }, []);
  const toggle = (c: string) => setContexts((cs) => (cs.includes(c) ? cs.filter((x) => x !== c) : [...cs, c]));
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      const all = [...contexts, ...custom.split(",").map((s) => s.trim()).filter(Boolean)];
      const p = await api.createProject({
        name, description,
        org: sharing === "org" ? org : undefined,
        share_users: sharing === "users" ? Object.fromEntries(shareUsers.map((u) => [u, "write"])) : {},
        philosophy: transcript ? undefined : philosophy || undefined,
        raw_transcript: transcript ?? undefined,
        contexts: all,
        template: template || undefined,
      });
      session.setProject(p.id);
      toast("success", `Created ${p.name}${p.kickoff ? ` — ${p.kickoff.length} kickoff work item(s) queued` : ""}`);
      onCreated(p.id);
    } catch (err) { toast("error", (err as Error).message); } finally { setBusy(false); }
  };
  return (
    <form className="card form" onSubmit={submit} data-testid="create-project">
      <div className="form-grid">
        <label className="form-field"><span className="label">1 · Name</span>
          <input className="input" value={name} onChange={(e) => setName(e.target.value)} required data-testid="project-name" /></label>
        <label className="form-field"><span className="label">Description</span>
          <input className="input" value={description} onChange={(e) => setDescription(e.target.value)} /></label>
      </div>
      <div className="form-field"><span className="label">2 · Sharing</span>
        <div className="seg">
          {(["private", "org", "users"] as const).map((s) => (
            <button type="button" key={s} className={sharing === s ? "on" : ""} onClick={() => setSharing(s)}>
              {s === "private" ? "Private" : s === "org" ? "An organization" : "Named users"}
            </button>
          ))}
        </div>
        {sharing === "org" && (orgs.length ? (
          <select className="input" value={org} onChange={(e) => setOrg(e.target.value)}>{orgs.map((o) => <option key={o._key} value={o._key}>{o.name}</option>)}</select>
        ) : <span className="faint">You are not in an organization yet (create one in Settings).</span>)}
        {sharing === "users" && (
          <div className="chips">{users.map((u) => (
            <label key={u.key} className="chip"><input type="checkbox" checked={shareUsers.includes(u.key)}
              onChange={() => setShareUsers((x) => (x.includes(u.key) ? x.filter((y) => y !== u.key) : [...x, u.key]))} /> {u.display_name}</label>
          ))}</div>
        )}
      </div>
      <div className="form-field"><span className="label">3 · Philosophy document — the vision and strategic goals; the tiebreaker for every agent</span>
        <textarea className="textarea" rows={4} value={transcript ?? philosophy} onChange={(e) => { setTranscript(null); setPhilosophy(e.target.value); }}
          placeholder="What is this product for, and what matters most when trade-offs appear?" data-testid="project-philosophy" />
        <Recorder onText={(t) => setTranscript(t)} />
        {transcript && <span className="faint">Transcribed — an architect agent will clean it up without losing meaning.</span>}
      </div>
      <div className="form-field"><span className="label">4 · Starting contexts</span>
        <div className="chips">
          {common.map((c) => (
            <label key={c.name} className="chip" title={c.description}>
              <input type="checkbox" checked={contexts.includes(c.name)} onChange={() => toggle(c.name)} data-testid={`ctx-${c.name}`} /> {c.name}
            </label>
          ))}
        </div>
        <input className="input" placeholder="Your own contexts, comma-separated (e.g. Capital Markets)" value={custom} onChange={(e) => setCustom(e.target.value)} />
      </div>
      <label className="form-field"><span className="label">5 · Template</span>
        <select className="input" value={template} onChange={(e) => setTemplate(e.target.value)} data-testid="project-template">
          <option value="">Start from scratch</option>
          <option value="master">Kodehub master template (best-practice contexts and requirements)</option>
          {projects.map((p) => <option key={p.id} value={`project:${p.id}`}>Copy contexts and requirements from {p.name}</option>)}
        </select>
      </label>
      <div className="row">
        <span className="spacer" />
        <button type="button" className="btn ghost-btn" onClick={onCancel}>Cancel</button>
        <button className="btn primary" disabled={busy || !name.trim()} data-testid="create-project-submit">Create project</button>
      </div>
    </form>
  );
}

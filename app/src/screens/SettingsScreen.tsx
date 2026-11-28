// Settings (spec §8.4, §11, §12, §13, §13A): how engaged the human wants to be, agent caps,
// retry limit, review, budget and backend; the test cadence and schedules; credentials; agent
// users and their tokens; Git import / export and backups. Values are layered: each shows where
// it comes from (app default, user / org, project).
import { useEffect, useState } from "react";
import { useToast } from "../components/Toasts";
import { api, type Json } from "../lib/api";
import { relTime } from "../lib/health";

const EDITABLE: { key: string; label: string; kind: "select" | "bool" | "number" | "json"; options?: string[] }[] = [
  { key: "human_engagement", label: "How engaged do you want to be?", kind: "select", options: ["contexts", "containers", "plans", "none"] },
  { key: "agent_backend", label: "Agent backend", kind: "select", options: ["claude_code", "api", "recorded"] },
  { key: "retry_limit", label: "Rework cycles before escalating to an architect", kind: "number" },
  { key: "blind_review", label: "Require a blind code review before promotion", kind: "bool" },
  { key: "budget_usd", label: "Model spend budget (USD, empty = none)", kind: "number" },
  { key: "agent_caps", label: "Agent concurrency caps", kind: "json" },
  { key: "test_cadence", label: "Test cadence", kind: "json" },
  { key: "test_budget_pct", label: "Largest share of the cluster for tests (%)", kind: "number" },
  { key: "use_case_check", label: "Architect checks use cases before they go live", kind: "bool" },
  { key: "kickoff", label: "New projects start the kickoff chain", kind: "bool" },
];

export function SettingsScreen({ project, me }: { project: string; me: Json }) {
  const toast = useToast();
  const scope = `project:${project}`;
  const [s, setS] = useState<Json>(null);
  const [schedules, setSchedules] = useState<Json[]>([]);
  const load = () => {
    api.scopeSettings(scope).then(setS).catch((e) => toast("error", e.message));
    api.schedules().then(setSchedules).catch(() => {});
  };
  useEffect(() => { load(); }, [project]); // eslint-disable-line react-hooks/exhaustive-deps
  const set = (key: string, value: Json) =>
    api.setSetting(scope, key, value).then((v) => { setS(v); toast("success", `${key} saved`); }).catch((e) => toast("error", e.message));
  return (
    <div className="screen settings-screen" data-testid="settings-screen">
      <div className="screen-head"><h2>Settings</h2><span className="faint">for this project · values fall back to the owner and then the app default</span></div>
      <div className="settings-grid">
        <div className="card">
          <div className="section-title">Project</div>
          {s && EDITABLE.map((d) => <SettingRow key={d.key} d={d} v={s.resolved?.[d.key]} editable={s.can_edit} onSave={(v) => set(d.key, v)}
            onClear={() => api.clearSetting(scope, d.key).then(setS)} />)}
        </div>
        <div className="card">
          <div className="section-title">Test schedule</div>
          <div className="faint" style={{ fontSize: 12 }}>Only nodes that are ready for test or live are scheduled. The daily full-project run is always kept.</div>
          {schedules.map((x) => (
            <div key={x._key} className="row sched" data-testid={`sched-${x.target}`}>
              <span className={`badge ${x.enabled ? "pass" : ""}`}>{x.enabled ? "on" : "off"}</span>
              <span className="mono">{x.target}</span>
              <span className="faint">every {Math.round(x.interval_secs / 360) / 10} h · next {x.next_due ? relTime(x.next_due) : "—"}</span>
              <span className="spacer" />
              {!x.always && <button className="btn sm ghost-btn" onClick={() => api.setScheduleEnabled(x._key, !x.enabled).then(load)}>{x.enabled ? "Pause" : "Resume"}</button>}
              {x.action === "test_run" || !x.action ? <button className="btn sm" onClick={() => api.testNow(x.target).then(() => toast("success", "Test run queued"))}>Test now</button> : null}
            </div>
          ))}
          <div className="row"><span className="spacer" /><button className="btn sm" onClick={() => api.replanSchedules().then(load)}>Re-plan from settings</button></div>
        </div>
        <AgentTypes editable={!!s?.can_edit} />
        <Credentials project={project} me={me} />
        <AgentUsers project={project} />
        <GitAndBackups />
      </div>
    </div>
  );
}

function SettingRow({ d, v, editable, onSave, onClear }: { d: (typeof EDITABLE)[number]; v: Json; editable: boolean; onSave: (v: Json) => void; onClear: () => void }) {
  const value = v?.value;
  const [draft, setDraft] = useState<string>(d.kind === "json" ? JSON.stringify(value) : String(value ?? ""));
  useEffect(() => { setDraft(d.kind === "json" ? JSON.stringify(value) : String(value ?? "")); }, [value, d.kind]);
  const save = () => {
    try {
      if (d.kind === "number") onSave(draft === "" ? null : Number(draft));
      else if (d.kind === "json") onSave(JSON.parse(draft));
      else onSave(draft);
    } catch { /* invalid JSON stays in the field */ }
  };
  return (
    <div className="setting-row" data-testid={`setting-${d.key}`}>
      <div><div>{d.label}</div><div className="faint" style={{ fontSize: 11 }}>from {v?.source ?? "default"}</div></div>
      <div className="row">
        {d.kind === "select" && (
          <select className="input" value={String(value)} disabled={!editable} onChange={(e) => onSave(e.target.value)}>
            {d.options!.map((o) => <option key={o} value={o}>{o}</option>)}
          </select>
        )}
        {d.kind === "bool" && <input type="checkbox" checked={value === true} disabled={!editable} onChange={(e) => onSave(e.target.checked)} />}
        {(d.kind === "number" || d.kind === "json") && (
          <>
            <input className="input" value={draft} disabled={!editable} onChange={(e) => setDraft(e.target.value)} style={{ minWidth: d.kind === "json" ? 260 : 80 }} />
            <button className="btn sm" disabled={!editable} onClick={save}>Save</button>
          </>
        )}
        {String(v?.source ?? "").startsWith("project:") && <button className="btn sm ghost-btn" onClick={onClear} title="Use the inherited value">Reset</button>}
      </div>
    </div>
  );
}

function Credentials({ project, me }: { project: string; me: Json }) {
  const toast = useToast();
  const user = me?.user?.key;
  const [mine, setMine] = useState<Json[]>([]);
  const [proj, setProj] = useState<Json[]>([]);
  const [kind, setKind] = useState("api_key");
  const [where, setWhere] = useState<"project" | "user">("project");
  const [secret, setSecret] = useState("");
  const load = () => {
    if (user) api.credentials(`user:${user}`).then(setMine).catch(() => {});
    api.credentials(`project:${project}`).then(setProj).catch(() => {});
  };
  useEffect(() => { load(); }, [project, user]); // eslint-disable-line react-hooks/exhaustive-deps
  const save = async () => {
    try {
      await api.putCredential({ scope: where === "user" ? `user:${user}` : `project:${project}`, provider: kind === "git_token" ? "git" : "anthropic", kind, secret });
      setSecret("");
      toast("success", "Stored encrypted");
      load();
    } catch (e) { toast("error", (e as Error).message); }
  };
  const row = (c: Json) => (
    <div key={c.id} className="row"><span className="chip">{c.kind.replace("_", " ")}</span><span className="faint">{c.scope} · …{c.fingerprint}</span><span className="spacer" />
      <button className="btn sm ghost-btn" onClick={() => api.deleteCredential(c.id).then(load)}>Remove</button></div>
  );
  return (
    <div className="card" data-testid="credentials">
      <div className="section-title">Credentials</div>
      <div className="faint" style={{ fontSize: 12 }}>Stored encrypted; never shown again, never logged. Agents receive them only for the duration of a task.</div>
      {[...proj, ...mine].map(row)}
      <div className="row">
        <select className="input" value={kind} onChange={(e) => { setKind(e.target.value); if (e.target.value === "subscription_token") setWhere("user"); }}>
          <option value="api_key">Anthropic API key</option>
          <option value="subscription_token">Claude subscription token (personal)</option>
          <option value="git_token">Git access token</option>
        </select>
        <select className="input" value={where} onChange={(e) => setWhere(e.target.value as "project" | "user")} disabled={kind === "subscription_token"}>
          <option value="project">this project</option>
          <option value="user">my account</option>
        </select>
      </div>
      {kind === "subscription_token" && (
        <div className="warn" data-testid="subscription-warning">
          Subscription tokens are for individual people working on personal projects, such as at home — not for production or
          for organizations, which must use API keys. Kodehub uses it only for projects you own yourself, only with the Claude
          Code CLI, and never sends it to the API. Check the provider's current terms before relying on it.
        </div>
      )}
      <div className="row">
        <input className="input" type="password" placeholder="Paste the secret" value={secret} onChange={(e) => setSecret(e.target.value)} autoComplete="off" />
        <button className="btn sm primary" disabled={!secret} onClick={save}>Store</button>
      </div>
    </div>
  );
}

/** The project's agent types (D-062): each has a start-up prompt and the actions it may take.
 *  Defaults are wide; review and test writing stay with their own agents. */
function AgentTypes({ editable }: { editable: boolean }) {
  const toast = useToast();
  const [agents, setAgents] = useState<Json[]>([]);
  const [catalogue, setCatalogue] = useState<{ name: string; description: string }[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const [draft, setDraft] = useState<{ prompt: string; permissions: string[] }>({ prompt: "", permissions: [] });
  const [custom, setCustom] = useState("");
  const load = () => api.agentTypes().then((r) => { setAgents(r.agents); setCatalogue(r.permissions); }).catch(() => {});
  useEffect(() => { load(); }, []);
  const edit = (a: Json) => {
    if (open === a.name) { setOpen(null); return; }
    setOpen(a.name);
    setDraft({ prompt: a.prompt ?? "", permissions: [...(a.permissions ?? [])] });
  };
  const save = (name: string) => api.putAgentType(name, draft)
    .then(() => { toast("success", `${name} saved`); setOpen(null); load(); })
    .catch((e) => toast("error", e.message));
  return (
    <div className="card" data-testid="agent-types">
      <div className="section-title">Agent types</div>
      <div className="faint" style={{ fontSize: 12 }}>What each kind of agent may do in this project, and how it starts. Whatever is allowed here, an agent never reviews its own work.</div>
      {agents.map((a) => (
        <div key={a._key}>
          <div className="row" data-testid={`agent-type-${a.name}`}>
            <b>{a.name}</b>{!a.builtin && <span className="chip">custom</span>}
            <span className="faint" style={{ fontSize: 11.5 }}>{(a.permissions ?? []).length} permission{(a.permissions ?? []).length === 1 ? "" : "s"}</span>
            <span className="spacer" />
            <button className="btn sm ghost-btn" onClick={() => edit(a)}>{open === a.name ? "Close" : editable ? "Edit" : "View"}</button>
          </div>
          {open === a.name && (
            <div style={{ padding: "4px 0 10px" }}>
              <label className="label">Start-up prompt{a.builtin ? " (added to the built-in role prompt)" : ""}</label>
              <textarea className="textarea" value={draft.prompt} disabled={!editable} onChange={(e) => setDraft({ ...draft, prompt: e.target.value })} />
              <label className="label">Permissions</label>
              <div className="perm-grid" style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(210px, 1fr))", gap: 4 }}>
                {catalogue.map((p) => (
                  <label key={p.name} className="row" style={{ fontSize: 12, gap: 6 }} title={p.description}>
                    <input type="checkbox" disabled={!editable} checked={draft.permissions.includes(p.name)}
                      onChange={(e) => setDraft({ ...draft, permissions: e.target.checked ? [...draft.permissions, p.name] : draft.permissions.filter((x) => x !== p.name) })} />
                    <span className="mono">{p.name}</span>
                  </label>
                ))}
              </div>
              {editable && <div className="row"><span className="spacer" /><button className="btn sm primary" onClick={() => save(a.name)} data-testid={`save-agent-${a.name}`}>Save</button></div>}
            </div>
          )}
        </div>
      ))}
      {editable && (
        <div className="row">
          <input className="input" placeholder="New custom agent, e.g. policy" value={custom} onChange={(e) => setCustom(e.target.value)} />
          <button className="btn sm" disabled={!custom.trim()} onClick={() => api.putAgentType(`custom:${custom.trim()}`, { prompt: `You are the ${custom.trim()} agent.`, permissions: ["create_work_items"] })
            .then(() => { setCustom(""); load(); }).catch((e) => toast("error", e.message))}>Add</button>
        </div>
      )}
    </div>
  );
}

function AgentUsers({ project }: { project: string }) {
  const toast = useToast();
  const [agents, setAgents] = useState<Json[]>([]);
  const [tokens, setTokens] = useState<Json[]>([]);
  const [name, setName] = useState("");
  const [shown, setShown] = useState<string | null>(null);
  const load = () => { api.agentUsers().then(setAgents).catch(() => {}); api.tokens().then(setTokens).catch(() => {}); };
  useEffect(() => { load(); }, []);
  return (
    <div className="card" data-testid="agent-users">
      <div className="section-title">External agents (MCP)</div>
      <div className="faint" style={{ fontSize: 12 }}>An assistant such as Claude Code acts as its own remote agent, linked to you — never as you. Remote agents may add work items, edit requirements and start tests; they never change code or deploy.</div>
      {agents.map((a) => (
        <div key={a.key} className="row">
          <b>{a.display_name}</b><span className="faint mono">{a.key}</span><span className="spacer" />
          <button className="btn sm" onClick={async () => {
            try { const t = await api.createToken(a.key, "mcp", [project], "write"); setShown(t.token); load(); } catch (e) { toast("error", (e as Error).message); }
          }}>New token for this project</button>
        </div>
      ))}
      {shown && (
        <div className="warn" data-testid="new-token">
          Shown once — paste it into your assistant's MCP settings as <code>KODEHUB_TOKEN</code>:
          <pre className="code-block">{shown}</pre>
          <button className="btn sm" onClick={() => setShown(null)}>Done</button>
        </div>
      )}
      {tokens.map((t) => (
        <div key={t.id} className="row faint"><span className="mono">{t.prefix}…</span><span>{t.user}</span><span>expires {relTime(t.expires_at)}</span><span className="spacer" />
          <button className="btn sm ghost-btn" onClick={() => api.revokeToken(t.id).then(load)}>Revoke</button></div>
      ))}
      <div className="row">
        <input className="input" placeholder="New agent user, e.g. Claude Code on my laptop" value={name} onChange={(e) => setName(e.target.value)} />
        <button className="btn sm" disabled={!name.trim()} onClick={() => api.createAgentUser(name).then(() => { setName(""); load(); }).catch((e) => toast("error", e.message))}>Add</button>
      </div>
    </div>
  );
}

function GitAndBackups() {
  const toast = useToast();
  const [url, setUrl] = useState("");
  const [exp, setExp] = useState("");
  return (
    <div className="card" data-testid="git">
      <div className="section-title">Git bridge and backups</div>
      <div className="faint" style={{ fontSize: 12 }}>Kodehub is the source of truth; Git is a one-time import and a mirror. Edits made in Git are not merged back.</div>
      <div className="row">
        <input className="input" placeholder="https://github.com/you/repo (one-time import)" value={url} onChange={(e) => setUrl(e.target.value)} />
        <button className="btn sm" disabled={!url} onClick={() => api.gitImport(url).then(() => toast("success", "Import queued")).catch((e) => toast("error", e.message))}>Import</button>
      </div>
      <div className="row">
        <input className="input" placeholder="https://github.com/you/mirror (export)" value={exp} onChange={(e) => setExp(e.target.value)} />
        <button className="btn sm" disabled={!exp} onClick={() => api.gitExport(exp, "main").then(() => toast("success", "Export queued")).catch((e) => toast("error", e.message))}>Export now</button>
      </div>
      <div className="row"><span className="spacer" />
        <button className="btn sm" onClick={() => api.backup().then((b) => toast("success", `Backed up (${Math.round(b.bytes / 1024)} KB)`)).catch((e) => toast("error", e.message))}>Back up this project now</button>
      </div>
    </div>
  );
}

import { useCallback, useEffect, useState } from "react";
import { api, pvKey, type Json, type ProjectVersion } from "../lib/api";
import { relTime } from "../lib/health";
import { Check, Chevron, Diamond, Rocket, Undo, X } from "./icons";
import { useToast } from "./Toasts";

/** Project versions are the releases (D-059): open while work goes in, frozen on their first
 *  deployment beyond the lowest environment (or by hand). Deployments carry one into one
 *  environment through its gate; environments that need a human wait for your approval here. */
export function ReleasesDrawer({ versions, environments, onClose, inline }: { versions: ProjectVersion[]; environments: Json[]; onClose: () => void; inline?: boolean }) {
  const toast = useToast();
  const [name, setName] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [gate, setGate] = useState<Json>(null);
  const [sel, setSel] = useState<string[]>([]);
  const [diff, setDiff] = useState<Json>(null);
  const [deployments, setDeployments] = useState<Json[]>([]);
  const envBy = new Map(environments.map((e) => [e.name, e]));
  // environments come from the project's settings (spec §16.1), lowest first
  const ENVS: string[] = [...environments].sort((a, b) => (a.order ?? 0) - (b.order ?? 0)).map((e) => e.name);
  const needsHuman = (env: string) => !!envBy.get(env)?.requires_human_approval;
  const nameOf = (id: string) => versions.find((v) => v._id === id)?.name ?? pvKey(id);
  const running = (env: string): Json[] => envBy.get(env)?.running ?? [];

  const loadDeployments = useCallback(() => {
    api.deployments().then(setDeployments).catch(() => setDeployments([]));
  }, []);
  useEffect(() => { loadDeployments(); }, [loadDeployments, versions, environments]);
  const waiting = deployments.filter((d) => d.state === "awaiting_approval");
  const failed = deployments.filter((d) => d.state === "failed");

  const act = async (key: string, f: () => Promise<void>) => {
    setBusy(key);
    try { await f(); } catch (e) { toast("error", String((e as Error).message)); } finally { setBusy(null); loadDeployments(); }
  };
  const report = (r: Json, env: string) => {
    if (r.gate) setGate({ ...r.gate, environment: r.gate.environment ?? env });
    if (r.awaiting_approval) toast("info", `Waiting for your approval to deploy into ${env}`);
    else toast(r.deployed ? "success" : "error", r.deployed ? `Deployed into ${env}` : `Gate for ${env} failed`);
  };
  const newVersion = () => act("new", async () => {
    const r = await api.newProjectVersion(name);
    toast("success", `Opened ${r.name} — new work goes into it`);
    setName("");
  });
  const deploy = (pv: string, env: string, reason = "promote") => act(env, async () => {
    if (needsHuman(env) && !window.confirm(`Deploy ${nameOf(pv)} into ${env.toUpperCase()}? It waits for your sign-off before it runs.`)) return;
    report(await api.deploy(pv, env, reason), env);
  });
  const approve = (d: Json) => act(`approve-${d._key}`, async () => {
    if (!window.confirm(`Approve deploying ${nameOf(d.project_version)} into ${String(d.environment).toUpperCase()}? This is your human sign-off.`)) return;
    report(await api.approveDeployment(d._key), d.environment);
  });
  const retry = (d: Json) => act(`retry-${d._key}`, async () => report(await api.retryDeployment(d._key), d.environment));
  const rollback = (env: string) => act(env, async () => {
    if (needsHuman(env) && !window.confirm(`Roll ${env.toUpperCase()} back to the version it ran before? It waits for your sign-off.`)) return;
    const r = await api.rollback(env);
    if (r.gate) setGate({ ...r.gate, environment: env });
    if (r.awaiting_approval) toast("info", `Rollback of ${env} to ${nameOf(r.rolled_back_to)} waits for your approval`);
    else toast(r.deployed ? "success" : "error", r.deployed ? `${env} rolled back to ${nameOf(r.rolled_back_to)}` : `Gate for ${env} failed`);
  });
  const freeze = (v: ProjectVersion) => act(`freeze-${v._key}`, async () => {
    await api.freezeVersion(v._id, "frozen by hand");
    toast("success", `${v.name} is frozen${v.current ? "; the next version is open" : ""}`);
  });
  const hotfix = (v: ProjectVersion) => act(`hotfix-${v._key}`, async () => {
    const r = await api.hotfix(v._id);
    toast("success", `Opened hotfix ${r.name} from ${v.name}`);
  });
  const makeCurrent = (v: ProjectVersion) => act(`current-${v._key}`, async () => {
    await api.makeCurrent(v._id);
    toast("success", `New work goes into ${v.name}`);
  });
  const toggle = async (id: string) => {
    const next = sel.includes(id) ? sel.filter((x) => x !== id) : [...sel, id].slice(-2);
    setSel(next);
    setDiff(null);
    if (next.length === 2) {
      // older first (versions come newest first)
      const ordered = versions.filter((v) => next.includes(v._id)).reverse().map((v) => v._id);
      setDiff(await api.diff(ordered[0], ordered[1]));
    }
  };

  return (
    <>
      {!inline && <div className="drawer-backdrop" onClick={onClose} />}
      <div className={inline ? "screen releases-screen" : "drawer right"} role={inline ? undefined : "dialog"} aria-label="Releases" data-testid="releases-drawer">
        <div className="drawer-head">
          <Rocket size={18} />
          <div style={{ flex: 1 }}>
            <div className="drawer-title">Project versions & environments</div>
            <div className="faint" style={{ fontSize: 12 }}>A project version is a release: open while work goes in, frozen once deployed beyond {ENVS[0] ?? "dev"}. {ENVS.filter(needsHuman).join(", ") || "prod"} needs your sign-off.</div>
          </div>
          {!inline && <button className="btn icon ghost-btn" aria-label="Close" onClick={onClose}><X size={15} /></button>}
        </div>
        <div className="drawer-body">
          <div className="envs" data-testid="env-pipeline">
            {ENVS.map((env, i) => {
              const cur = running(env);
              const top = cur[0];
              // the newest version running one environment down that is not running here yet
              const below = i > 0 ? running(ENVS[i - 1]).find((d) => !cur.some((c) => c.project_version === d.project_version)) : null;
              return (
                <div className="card env" key={env} data-testid={`env-${env}`}>
                  <div className="env-name"><span>{env}</span>{top && <span className="dot" style={{ background: "var(--pass)" }} />}</div>
                  <div className="env-rel" data-testid={`env-${env}-release`}>{top ? cur.map((d) => nameOf(d.project_version)).join(", ") : <span className="faint">—</span>}</div>
                  <div className="env-since">{top ? relTime(top.finished_at) : i === 0 ? "deploy a version" : "nothing deployed"}</div>
                  <div className="env-actions">
                    {below && (
                      <button className="btn sm primary" disabled={!!busy} onClick={() => deploy(below.project_version, env)} data-testid={`promote-${env}`}>
                        {busy === env ? <span className="spin" /> : <Chevron size={11} />} Deploy {nameOf(below.project_version)}
                      </button>
                    )}
                    {top && i > 0 && (
                      <button className="btn sm" disabled={!!busy} onClick={() => rollback(env)} data-testid={`rollback-${env}`}><Undo size={11} /> Rollback</button>
                    )}
                  </div>
                  {i < ENVS.length - 1 && <span className="arrow"><Chevron size={14} /></span>}
                </div>
              );
            })}
          </div>
          {waiting.length > 0 && (
            <div className="card" style={{ padding: 12, marginTop: 12 }} data-testid="awaiting-approval">
              <b>Waiting for your approval</b>
              {waiting.map((d) => (
                <div className="row" key={d._key} style={{ marginTop: 6 }}>
                  <span style={{ flex: 1 }}>{nameOf(d.project_version)} → <b>{d.environment}</b> <span className="faint">{d.reason} · {relTime(d.created_at)}</span></span>
                  <button className="btn sm primary" disabled={!!busy} onClick={() => approve(d)} data-testid={`approve-${d.environment}`}>
                    {busy === `approve-${d._key}` ? <span className="spin" /> : <Check size={11} />} Approve
                  </button>
                </div>
              ))}
            </div>
          )}
          {failed.length > 0 && (
            <div className="card" style={{ padding: 12, marginTop: 12 }} data-testid="failed-deployments">
              <b>Failed deployments</b>
              {failed.slice(0, 5).map((d) => (
                <div className="row" key={d._key} style={{ marginTop: 6 }}>
                  <span style={{ flex: 1 }}>{nameOf(d.project_version)} → <b>{d.environment}</b> <span className="faint">{d.attempts} attempt{d.attempts === 1 ? "" : "s"} · {relTime(d.finished_at ?? d.created_at)}</span></span>
                  <button className="btn sm" disabled={!!busy} onClick={() => retry(d)} data-testid={`retry-${d.environment}`}>Retry</button>
                </div>
              ))}
            </div>
          )}
          {gate && (
            <div className="gate" data-testid="gate-report">
              <b>Gate · {gate.environment}</b> — {gate.ok ? <span style={{ color: "var(--pass)" }}>passed</span> : <span style={{ color: "var(--fail)" }}>failed</span>}
              {(gate.checks ?? []).map((c: Json, i: number) => (
                <div className="gate-check" key={i}>
                  {c.ok ? <Check size={12} style={{ color: "var(--pass)" }} /> : <X size={12} style={{ color: "var(--fail)" }} />}
                  <span>{c.check}</span>
                  {c.total !== undefined && <span className="faint">({c.total - (c.failed?.length ?? 0)}/{c.total})</span>}
                </div>
              ))}
              {gate.error && <div className="faint">{gate.error}</div>}
            </div>
          )}

          <div className="row" style={{ marginTop: 18 }}>
            <input className="input" placeholder="Name of the next version (optional)" value={name} onChange={(e) => setName(e.target.value)} data-testid="release-name" />
            <button className="btn primary" onClick={newVersion} disabled={busy === "new"} data-testid="cut-release" title="Open a new project version from the current one; new work goes into it">
              {busy === "new" ? <span className="spin" /> : <Diamond size={12} />} New version
            </button>
          </div>

          <div className="section-head" style={{ padding: "16px 0 6px" }}>
            <span className="section-title">Project versions</span>
            <span className="faint" style={{ fontSize: 11.5 }}>select two to diff</span>
          </div>
          <div className="card" style={{ overflow: "hidden" }}>
            {versions.length === 0 && <div className="empty">No project versions yet.</div>}
            {versions.map((v) => (
              <div className="release-row" key={v._key} data-testid={`release-${v.name}`}>
                <input type="checkbox" checked={sel.includes(v._id)} onChange={() => toggle(v._id)} aria-label={`select ${v.name}`} />
                <div>
                  <div style={{ fontWeight: 600 }}>{v.name}{v.current && <span className="badge ghost" style={{ marginLeft: 6 }}>current</span>}</div>
                  <div className="faint" style={{ fontSize: 11.5 }}>
                    {relTime(v.frozen_at ?? v.created_at)}
                    {v.based_on ? <> · from <span className="mono">{nameOf(v.based_on)}</span></> : null}
                    {v.hotfix_of ? <> · hotfix of <span className="mono">{nameOf(v.hotfix_of)}</span></> : null}
                  </div>
                </div>
                <div className="row" style={{ gap: 4 }}>
                  <span className={`badge ${v.state === "open" ? "risk" : "pass"}`} data-testid={`release-state-${v.name}`}>{v.state}</span>
                  {v.running_in?.map((e: string) => <span className="badge pass" key={e}>{e}</span>)}
                  {v.state === "open" && ENVS[0] && !v.running_in?.includes(ENVS[0]) && (
                    <button className="btn sm ghost-btn" disabled={!!busy} onClick={() => deploy(v._id, ENVS[0])} data-testid={`deploy-${v.name}`}>Deploy to {ENVS[0]}</button>
                  )}
                  {v.state === "open" && !v.current && (
                    <button className="btn sm ghost-btn" disabled={!!busy} onClick={() => makeCurrent(v)} title="New work goes into this version">Make current</button>
                  )}
                  {v.state === "open" && (
                    <button className="btn sm ghost-btn" disabled={!!busy} onClick={() => freeze(v)} data-testid={`freeze-${v.name}`} title="Freeze it: it never changes again">Freeze</button>
                  )}
                  {v.state === "frozen" && (
                    <button className="btn sm ghost-btn" disabled={!!busy} onClick={() => hotfix(v)} data-testid={`hotfix-${v.name}`} title="Open a hotfix version starting from this one">Hotfix</button>
                  )}
                </div>
              </div>
            ))}
          </div>
          {diff && (
            <div className="card" style={{ padding: 12, marginTop: 12 }} data-testid="release-diff">
              <b>Diff</b> <span className="faint mono">{nameOf(diff.from)} → {nameOf(diff.to)}</span>
              <div className="diff-list" style={{ marginTop: 8 }}>
                {diff.changed.map((c: Json) => <div className="chg" key={c.id}>~ {c.name} <span className="faint">{c.level}</span></div>)}
                {diff.added.map((c: Json) => <div className="add" key={c.id}>+ {c.name} <span className="faint">{c.level}</span></div>)}
                {diff.removed.map((c: Json) => <div className="rem" key={c.id}>− {c.name} <span className="faint">{c.level}</span></div>)}
                {!diff.changed.length && !diff.added.length && !diff.removed.length && <div className="faint">identical</div>}
              </div>
            </div>
          )}
        </div>
      </div>
    </>
  );
}

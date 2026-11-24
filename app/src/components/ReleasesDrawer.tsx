import { useState } from "react";
import { api, type Json } from "../lib/api";
import { relTime } from "../lib/health";
import { Check, Chevron, Diamond, Rocket, Undo, X } from "./icons";
import { useToast } from "./Toasts";

export function ReleasesDrawer({ releases, environments, onClose, inline }: { releases: Json[]; environments: Json[]; onClose: () => void; inline?: boolean }) {
  const toast = useToast();
  const [name, setName] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [gate, setGate] = useState<Json>(null);
  const [sel, setSel] = useState<string[]>([]);
  const [diff, setDiff] = useState<Json>(null);
  const envBy = new Map(environments.map((e) => [e._key, e]));
  // environments come from the project's settings (spec §16.1), lowest first
  const ENVS: string[] = [...environments].sort((a, b) => (a.order ?? 0) - (b.order ?? 0)).map((e) => e._key);
  const needsHuman = (env: string) => !!envBy.get(env)?.requires_human_approval;

  const cut = async () => {
    setBusy("cut");
    try {
      const r = await api.cutRelease(name, "");
      toast("success", `Created ${r.name} — open in ${r.environment ?? ENVS[0]}`);
      setName("");
    } catch (e) { toast("error", String((e as Error).message)); } finally { setBusy(null); }
  };
  const promote = async (env: string, release: string) => {
    setBusy(env);
    try {
      let approval: string | undefined;
      if (needsHuman(env)) {
        if (!window.confirm(`Promote ${release} to ${env.toUpperCase()}? This is your human sign-off.`)) { setBusy(null); return; }
        approval = (await api.approveProd(release, env)).approval;
      }
      const r = await api.promote(env, release, approval);
      setGate(r.gate);
      toast(r.promoted ? "success" : "error", r.promoted ? `Promoted to ${env}` : `Gate for ${env} failed`);
    } catch (e) { toast("error", String((e as Error).message)); } finally { setBusy(null); }
  };
  const rollback = async (env: string) => {
    setBusy(env);
    try {
      let approval: string | undefined;
      const target = envBy.get(env)?.history?.[0]?.release;
      if (needsHuman(env) && target) {
        if (!window.confirm(`Roll ${env.toUpperCase()} back to ${target}? This is your human sign-off.`)) { setBusy(null); return; }
        approval = (await api.approveProd(target, env)).approval;
      }
      const r = await api.rollback(env, approval);
      toast(r.rolled_back ? "success" : "error", r.rolled_back ? `${env} rolled back to ${r.rolled_back_to}` : `Gate for ${env} failed`);
      if (r.gate) setGate(r.gate);
    } catch (e) { toast("error", String((e as Error).message)); } finally { setBusy(null); }
  };
  const cloneToDev = async (k: string) => {
    setBusy(`clone-${k}`);
    try {
      const r = await api.cloneRelease(k, ENVS[0]);
      toast("success", `Cloned ${k} → ${r._key} in ${ENVS[0]}`);
    } catch (e) { toast("error", String((e as Error).message)); } finally { setBusy(null); }
  };
  const toggle = async (k: string) => {
    const next = sel.includes(k) ? sel.filter((x) => x !== k) : [...sel, k].slice(-2);
    setSel(next);
    setDiff(null);
    if (next.length === 2) {
      const ordered = releases.filter((r) => next.includes(r._key)).reverse().map((r) => r._key);
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
            <div className="drawer-title">Releases & environments</div>
            <div className="faint" style={{ fontSize: 12 }}>Open in {ENVS[0] ?? "dev"}, immutable once promoted, moved through gated environments. {ENVS.filter(needsHuman).join(", ") || "prod"} needs your sign-off.</div>
          </div>
          {!inline && <button className="btn icon ghost-btn" aria-label="Close" onClick={onClose}><X size={15} /></button>}
        </div>
        <div className="drawer-body">
          <div className="envs" data-testid="env-pipeline">
            {ENVS.map((env, i) => {
              const e = envBy.get(env);
              const cur = e?.currently_running;
              const prevCur = i > 0 ? envBy.get(ENVS[i - 1])?.currently_running : null;
              const canPromote = i > 0 && prevCur && prevCur.release !== cur?.release;
              return (
                <div className="card env" key={env} data-testid={`env-${env}`}>
                  <div className="env-name"><span>{env}</span>{cur && <span className="dot" style={{ background: "var(--pass)" }} />}</div>
                  <div className="env-rel" data-testid={`env-${env}-release`}>{cur ? cur.name : <span className="faint">—</span>}</div>
                  <div className="env-since">{cur ? relTime(cur.since) : env === "dev" ? "cut a release" : "nothing deployed"}</div>
                  <div className="env-actions">
                    {canPromote && (
                      <button className="btn sm primary" disabled={!!busy} onClick={() => promote(env, prevCur.release)} data-testid={`promote-${env}`}>
                        {busy === env ? <span className="spin" /> : <Chevron size={11} />} Promote {prevCur.name}
                      </button>
                    )}
                    {e?.history?.length > 0 && i > 0 && (
                      <button className="btn sm" disabled={!!busy} onClick={() => rollback(env)} data-testid={`rollback-${env}`}><Undo size={11} /> Rollback</button>
                    )}
                  </div>
                  {e?.releases?.length > 1 && <div className="faint" style={{ fontSize: 11 }}>+{e.releases.length - 1} more</div>}
                  {i < ENVS.length - 1 && <span className="arrow"><Chevron size={14} /></span>}
                </div>
              );
            })}
          </div>
          {gate && (
            <div className="gate" data-testid="gate-report">
              <b>Gate · {gate.environment}</b> — {gate.ok ? <span style={{ color: "var(--pass)" }}>passed</span> : <span style={{ color: "var(--fail)" }}>failed</span>}
              {gate.checks.map((c: Json, i: number) => (
                <div className="gate-check" key={i}>
                  {c.ok ? <Check size={12} style={{ color: "var(--pass)" }} /> : <X size={12} style={{ color: "var(--fail)" }} />}
                  <span>{c.check}</span>
                  {c.total !== undefined && <span className="faint">({c.total - (c.failed?.length ?? 0)}/{c.total})</span>}
                </div>
              ))}
            </div>
          )}

          <div className="row" style={{ marginTop: 18 }}>
            <input className="input" placeholder="Release name (optional)" value={name} onChange={(e) => setName(e.target.value)} data-testid="release-name" />
            <button className="btn primary" onClick={cut} disabled={busy === "cut"} data-testid="cut-release">
              {busy === "cut" ? <span className="spin" /> : <Diamond size={12} />} Cut release
            </button>
          </div>

          <div className="section-head" style={{ padding: "16px 0 6px" }}>
            <span className="section-title">Releases</span>
            <span className="faint" style={{ fontSize: 11.5 }}>select two to diff</span>
          </div>
          <div className="card" style={{ overflow: "hidden" }}>
            {releases.length === 0 && <div className="empty">No releases yet.</div>}
            {releases.map((r) => (
              <div className="release-row" key={r._key} data-testid={`release-${r._key}`}>
                <input type="checkbox" checked={sel.includes(r._key)} onChange={() => toggle(r._key)} aria-label={`select ${r.name}`} />
                <div>
                  <div style={{ fontWeight: 600 }}>{r.name}</div>
                  <div className="faint" style={{ fontSize: 11.5 }}>{r.symbols} symbols · {relTime(r.created_at)} · <span className="mono">{r._key}</span>{r.cloned_from ? <> · from <span className="mono">{r.cloned_from}</span></> : null}</div>
                </div>
                <div className="row" style={{ gap: 4 }}>
                  <span className={`badge ${r.state === "retired" ? "" : r.state === "open" ? "risk" : "pass"}`} data-testid={`release-state-${r._key}`}>{r.state}</span>
                  {r.running_in?.map((e: string) => <span className="badge pass" key={e}>{e}</span>)}
                  <button className="btn sm ghost-btn" disabled={!!busy} onClick={() => cloneToDev(r._key)} title={`Clone into ${ENVS[0]}`} data-testid={`clone-${r._key}`}>Clone</button>
                </div>
              </div>
            ))}
          </div>
          {diff && (
            <div className="card" style={{ padding: 12, marginTop: 12 }} data-testid="release-diff">
              <b>Diff</b> <span className="faint mono">{diff.from} → {diff.to}</span>
              <div className="diff-list" style={{ marginTop: 8 }}>
                {diff.changed.map((c: Json) => <div className="chg" key={c.symbol}>~ {c.display_name} <span className="faint">{c.from.slice(0, 8)} → {c.to.slice(0, 8)}</span></div>)}
                {diff.added.map((c: Json) => <div className="add" key={c.symbol}>+ {c.display_name}</div>)}
                {diff.removed.map((c: Json) => <div className="rem" key={c.symbol}>− {c.display_name}</div>)}
                {!diff.changed.length && !diff.added.length && !diff.removed.length && <div className="faint">identical</div>}
              </div>
            </div>
          )}
        </div>
      </div>
    </>
  );
}

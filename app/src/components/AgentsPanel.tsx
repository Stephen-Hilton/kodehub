import { useEffect, useRef, useState } from "react";
import { api, localKey, type Json, type Proposal } from "../lib/api";
import { relTime, STATE_ORDER, stateIndex } from "../lib/health";
import { Bot, Check, Link, Scale, Send, X } from "./icons";
import { useToast } from "./Toasts";

const ROLE_COLORS: Record<string, string> = {
  orchestrator: "#8b9bff", tester: "#5eead4", coder: "#fbbf24", reviewer: "#f472b6",
};

type Todo = { label: string; done: boolean; now?: boolean };

/** Per-agent checklist for the active proposal (doc 05: status, current task and to-do list). */
export function agentTodos(cs: Proposal | undefined): Record<string, Todo[]> {
  if (!cs) return {};
  const props = Object.values(cs.proposals ?? {});
  const code = props.filter((p: Json) => p.kind !== "test").length;
  const tests = props.filter((p: Json) => p.kind === "test").length;
  const reviews = cs.reviews?.length ?? 0;
  const i = stateIndex(cs.state);
  const ran = (cs.test_results?.length ?? 0) > 0;
  return {
    orchestrator: [
      { label: "plan", done: !!cs.plan, now: cs.state === "draft" },
      { label: "your approval", done: i >= stateIndex("approved"), now: cs.state === "planned" },
      { label: "decisions", done: (cs.decisions?.length ?? 0) > 0 || cs.state === "accepted" },
      { label: "accept", done: cs.state === "accepted", now: cs.state === "ready" },
    ],
    tester: [
      { label: `tests from requirements${tests ? ` (${tests})` : ""}`, done: tests > 0, now: cs.state === "building" && tests === 0 },
      { label: "run affected tests", done: ran && i >= stateIndex("ready"), now: cs.state === "testing" },
    ],
    coder: [
      { label: `implement${code ? ` (${code})` : ""}`, done: code > 0, now: cs.state === "building" && code === 0 },
      ...((cs.rounds ?? 0) > 0 ? [{ label: `rework ×${cs.rounds}`, done: cs.state !== "building", now: cs.state === "building" }] : []),
    ],
    reviewer: [
      { label: `blind review${reviews ? ` (${reviews} round${reviews > 1 ? "s" : ""})` : ""}`, done: reviews > 0 && cs.state !== "reviewing", now: cs.state === "reviewing" },
    ],
  };
}

function Team({ agents, active }: { agents: Json; active?: Proposal }) {
  const todos = agentTodos(active);
  return (
    <div style={{ padding: "4px 10px 8px" }} data-testid="agent-team">
      {(agents?.agents ?? []).map((a: Json) => {
        const c = ROLE_COLORS[a.role] ?? "#94a3b8";
        const working = a.state === "working";
        const list = todos[a.role] ?? [];
        return (
          <div className="agent" key={a.role}>
            <div className={`avatar ${working ? "working" : ""}`} style={{ background: `${c}22`, color: c }}>{a.role[0].toUpperCase()}</div>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div className="row"><span className="agent-name">{a.role}</span>{a.role === "reviewer" && <span className="chip" title="Sees only requirements, mission, diff and tests">blind</span>}</div>
              <div className="agent-task">{working ? a.task : list.find((t) => t.now)?.label ?? (a.task ? `idle · last: ${a.task}` : "idle")}</div>
              {list.length > 0 && (
                <div className="todos">
                  {list.map((t, i) => <span key={i} className={`todo ${t.done ? "done" : ""} ${t.now ? "now" : ""}`}>{t.done ? "✓" : t.now ? "●" : "○"} {t.label}</span>)}
                </div>
              )}
            </div>
            <span className={`badge ${working ? "ghost" : "neutral"}`}>{working ? "working" : "idle"}</span>
          </div>
        );
      })}
    </div>
  );
}

function Pipeline({ state }: { state: string }) {
  const idx = stateIndex(state);
  const terminal = state === "escalated" || state === "rejected";
  return (
    <>
      <div className={`pipeline ${state}`} title={state}>
        {STATE_ORDER.slice(1).map((s, i) => {
          const pos = i + 1;
          const cls = state === "accepted" ? "done" : pos < idx ? "done" : pos === idx || (terminal && pos === 1) ? "now" : "";
          return <div key={s} className={cls} />;
        })}
      </div>
      <div className="pipeline-labels"><span>plan</span><span>approve</span><span>build</span><span>review</span><span>test</span><span>accept</span></div>
    </>
  );
}

/** Confidence flags shown on every card face. */
function Flags({ c }: { c: Proposal }) {
  const latest = new Map<string, Json>();
  for (const r of c.reviews ?? []) for (const f of r.findings ?? []) latest.set(f.id, f);
  const findings = [...latest.values()];
  const blockingOpen = findings.filter((f) => f.severity === "blocking" && f.status === "open").length;
  const blockingResolved = findings.filter((f) => (f.severity === "blocking" || f.severity === "major") && f.status === "resolved").length;
  const last = c.reviews?.[c.reviews.length - 1];
  const outOfPlan = (c.plan?.changes ?? []).filter((x: Json) => x.out_of_plan).length;
  const passed = (c.test_results ?? []).filter((t: Json) => t.status === "pass").length;
  const total = c.test_results?.length ?? 0;
  if (!last && !total && !outOfPlan) return null;
  return (
    <div className="flags" data-testid="cs-flags">
      {last && <span className={`badge ${last.findings?.some((f: Json) => f.status === "open" && (f.severity === "blocking" || f.severity === "major")) ? "at_risk" : "pass"}`}>
        review r{c.reviews.length}: {last.findings?.some((f: Json) => f.status === "open" && (f.severity === "blocking" || f.severity === "major")) ? "changes requested" : "approved"}</span>}
      {blockingOpen > 0 && <span className="badge fail">{blockingOpen} blocking open</span>}
      {blockingResolved > 0 && <span className="badge neutral">{blockingResolved} finding{blockingResolved > 1 ? "s" : ""} resolved</span>}
      {total > 0 && <span className={`badge ${passed === total ? "pass" : "fail"}`}>{passed}/{total} tests</span>}
      {(c.decisions?.length ?? 0) > 0 && <span className="chip"><Scale size={11} /> {c.decisions.length} decision{c.decisions.length > 1 ? "s" : ""}</span>}
      {outOfPlan > 0 && <span className="badge at_risk" title="The coder changed symbols outside the approved plan">{outOfPlan} out of plan</span>}
    </div>
  );
}

/** Human resolution of an escalated proposal (send back with guidance, or accept and re-test). */
function Resolve({ cs }: { cs: Proposal }) {
  const toast = useToast();
  const [choice, setChoice] = useState("");
  const [busy, setBusy] = useState(false);
  const go = async (to: "building" | "testing") => {
    setBusy(true);
    try {
      await api.resolve(cs._key, choice || (to === "testing" ? "Accept the code as is" : "Address the open findings"), "Resolved by the human after escalation", to);
      toast("success", to === "testing" ? "Accepted — re-testing" : "Sent back to the coder with your decision");
    } catch (e) { toast("error", String((e as Error).message)); } finally { setBusy(false); }
  };
  return (
    <div className="finding blocking" style={{ marginTop: 8 }} data-testid="resolve">
      <b>Agents could not agree — your call.</b>
      <textarea className="textarea" style={{ marginTop: 6 }} placeholder="Your decision (recorded as a Decision)…" value={choice} onChange={(e) => setChoice(e.target.value)} />
      <div className="row" style={{ marginTop: 6 }}>
        <button className="btn sm" disabled={busy} onClick={() => go("building")}>Send back to coder</button>
        <button className="btn sm primary" disabled={busy} onClick={() => go("testing")}>Accept &amp; re-test</button>
      </div>
    </div>
  );
}

export function ProposalCard({ cs, requirementsTitle }: { cs: Proposal; requirementsTitle: (k: string) => string }) {
  const toast = useToast();
  const [full, setFull] = useState<Proposal | null>(null);
  const [busy, setBusy] = useState(false);
  const [showLog, setShowLog] = useState(false);
  const closed = ["accepted", "rejected"].includes(cs.state);
  const [expanded, setExpanded] = useState(!closed);
  useEffect(() => { api.proposal(cs._key).then(setFull).catch(() => {}); }, [cs]);
  const c = full ?? cs;
  const act = async (f: () => Promise<unknown>, ok: string) => {
    setBusy(true);
    try { await f(); toast("success", ok); } catch (e) { toast("error", String((e as Error).message)); } finally { setBusy(false); }
  };
  const proposals = Object.entries(c.proposals ?? {});
  const code = proposals.filter(([, p]) => p.kind !== "test");
  const tests = proposals.filter(([, p]) => p.kind === "test");
  const lastReview = c.reviews?.[c.reviews.length - 1];
  const allFindings: Json[] = c.reviews?.flatMap((r) => r.findings) ?? [];
  const uniqueFindings = [...new Map(allFindings.map((f) => [f.id, f])).values()];
  const passed = (c.test_results ?? []).filter((t: Json) => t.status === "pass").length;
  const stateBadge = c.state === "accepted" ? "pass" : c.state === "escalated" || c.state === "rejected" ? "fail" : c.state === "planned" ? "at_risk" : "ghost";
  return (
    <div className="card cs" data-testid={`proposal-${c._key}`} data-state={c.state}>
      <div className="row" style={{ alignItems: "flex-start" }}>
        <div className="cs-title" style={{ flex: 1 }}>{c.title}</div>
        <span className={`badge ${stateBadge}`} data-testid="cs-state">{c.state}</span>
      </div>
      <div className="row" style={{ marginTop: 6, flexWrap: "wrap", gap: 4 }}>
        {c.requirements.map((r) => <span className="chip" key={r}>{requirementsTitle(r)}</span>)}
        <span className="faint" style={{ fontSize: 11 }}>{relTime(c.accepted_at ?? c.created_at)}</span>
      </div>
      <Pipeline state={c.state} />
      <Flags c={c} />
      {c.state === "escalated" && <Resolve cs={c} />}
      {closed && !expanded && (
        <div className="row" style={{ marginTop: 8, gap: 6, flexWrap: "wrap" }}>
          {(c.test_results?.length ?? 0) > 0 && <span className="badge pass">{(c.test_results ?? []).filter((t: Json) => t.status === "pass").length}/{c.test_results.length} tests</span>}
          {(c.reviews?.length ?? 0) > 0 && <span className="chip">{c.reviews.length} review round{c.reviews.length > 1 ? "s" : ""}</span>}
          {(c.decisions?.length ?? 0) > 0 && <span className="chip"><Scale size={11} /> {c.decisions.length} decision{c.decisions.length > 1 ? "s" : ""}</span>}
          <span className="spacer" />
          <button className="btn sm ghost-btn" onClick={() => setExpanded(true)}>Details</button>
        </div>
      )}
      {expanded && <>

      {c.plan && (
        <div style={{ marginTop: 10 }}>
          <div className="muted" style={{ fontSize: 12.5 }}>{c.plan.summary}</div>
          <div style={{ marginTop: 6 }}>
            {(c.plan.changes ?? []).map((ch: Json, i: number) => (
              <div className="plan-change" key={i}>
                <span className={`badge ${ch.action === "add" ? "ghost" : "at_risk"}`}>{ch.action}</span>
                <span className="mono">{ch.display_name}</span>
                <span className="faint" style={{ fontSize: 11.5 }}>{ch.description}</span>
              </div>
            ))}
          </div>
          {c.plan.risks?.length > 0 && <div className="faint" style={{ fontSize: 11.5, marginTop: 6 }}>Risks: {c.plan.risks.join(" · ")}</div>}
        </div>
      )}

      {c.state === "planned" && (
        <div className="row" style={{ marginTop: 10 }}>
          <button className="btn primary sm" disabled={busy} data-testid="cs-approve"
            onClick={() => act(() => api.approve(c._key), "Plan approved — tester, coder and reviewer are on it.")}>
            <Check size={12} /> Approve plan
          </button>
          <button className="btn sm danger" disabled={busy} onClick={() => act(() => api.reject(c._key, "rejected by human"), "Proposal rejected")}>
            <X size={12} /> Reject
          </button>
        </div>
      )}
      {c.state === "ready" && (
        <div className="row" style={{ marginTop: 10 }}>
          <button className="btn good sm" disabled={busy} onClick={() => act(() => api.accept(c._key), "Accepted into the project version")} data-testid="cs-accept">Accept</button>
        </div>
      )}
      {c.error && (
        <div className="finding blocking" style={{ marginTop: 8 }}>
          <b>Stopped:</b> {c.error}
          <div style={{ marginTop: 6 }}><button className="btn sm" onClick={() => act(() => api.resume(c._key), "Resumed")}>Resume</button></div>
        </div>
      )}

      {(code.length > 0 || tests.length > 0) && (
        <div style={{ marginTop: 10 }}>
          <div className="section-title" style={{ marginBottom: 4 }}>Proposals</div>
          {[...code, ...tests].map(([k, p]) => (
            <div className="plan-change" key={k}>
              <span className={`badge ${p.new ? "ghost" : "at_risk"}`}>{p.new ? "new" : "change"}</span>
              <span className="mono">{p.display_name}</span>
              <span className="faint" style={{ fontSize: 11 }}>{p.kind} · by {p.by}</span>
            </div>
          ))}
        </div>
      )}

      {uniqueFindings.length > 0 && (
        <div style={{ marginTop: 10 }} data-testid="findings">
          <div className="section-title" style={{ marginBottom: 2 }}>Blind review · round {c.reviews.length}{lastReview?.summary ? "" : ""}</div>
          {uniqueFindings.map((f) => (
            <div key={f.id} className={`finding ${f.severity} ${f.status}`}>
              <div className="row"><span className={`badge ${f.severity === "blocking" ? "fail" : f.severity === "major" ? "at_risk" : "neutral"}`}>{f.severity}</span>
                <span className="mono faint" style={{ fontSize: 11 }}>{f.location}</span><span className="spacer" />
                <span className={`badge ${f.status === "resolved" ? "pass" : "neutral"}`}>{f.status}</span></div>
              <div style={{ marginTop: 4 }}>{f.finding}</div>
            </div>
          ))}
        </div>
      )}

      {(c.test_results?.length ?? 0) > 0 && (
        <div className="row" style={{ marginTop: 10, fontSize: 12 }}>
          <span className={`badge ${passed === c.test_results.length ? "pass" : "fail"}`}>{passed}/{c.test_results.length} tests pass</span>
          {c.decisions?.length > 0 && <span className="chip"><Scale size={11} /> {c.decisions.length} decision{c.decisions.length > 1 ? "s" : ""} recorded</span>}
        </div>
      )}

      {(c.activity?.length ?? 0) > 0 && (
        <div style={{ marginTop: 10 }}>
          <button className="btn sm ghost-btn" onClick={() => setShowLog(!showLog)}>{showLog ? "Hide" : "Show"} activity ({c.activity.length})</button>
          {showLog && (
            <div style={{ marginTop: 6 }}>
              {c.activity.slice(-14).map((a, i) => (
                <div className="activity" key={i}><span className="who" style={{ color: ROLE_COLORS[a.role] }}>{a.role}</span><span style={{ flex: 1 }}>{a.message}</span></div>
              ))}
            </div>
          )}
        </div>
      )}
      {closed && <button className="btn sm ghost-btn" style={{ marginTop: 6 }} onClick={() => setExpanded(false)}>Collapse</button>}
      </>}
    </div>
  );
}

function Chat({ selectedNode, onClearRef }: { selectedNode: string | null; onClearRef: () => void }) {
  const toast = useToast();
  const [msgs, setMsgs] = useState<Json[]>([]);
  const [text, setText] = useState("");
  const [refs, setRefs] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const log = useRef<HTMLDivElement>(null);
  useEffect(() => { api.chatHistory().then(setMsgs).catch(() => {}); }, []);
  useEffect(() => { log.current?.scrollTo({ top: 1e9, behavior: "smooth" }); }, [msgs, busy]);
  const send = async () => {
    const m = text.trim();
    if (!m) return;
    setMsgs((x) => [...x, { role: "human", text: m, references: refs }]);
    setText(""); setBusy(true);
    try {
      const r = await api.chat(m, refs);
      setMsgs((x) => [...x, r]);
      setRefs([]);
      if (r.status === "pending") toast("info", "The orchestrator proposes a change — review it and click Apply.");
    } catch (e) {
      setMsgs((x) => [...x, { role: "orchestrator", text: `⚠ ${String((e as Error).message)}` }]);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="chat" data-testid="chat">
      <div className="chat-log" ref={log}>
        {msgs.length === 0 && (
          <div className="empty">
            <Bot size={22} /><br />Ask the orchestrator for new work.<br />
            <span className="faint">e.g. “Locked accounts should fail the same way as wrong passwords.”</span>
          </div>
        )}
        {msgs.map((m, i) => (
          <div key={i} className={`msg ${m.role}`}>
            {m.text}
            {m.references?.length > 0 && <div style={{ marginTop: 6 }}>{m.references.map((r: string) => <span key={r} className="chip" style={{ marginRight: 4 }}><Link size={10} /> {r.split("/")[1]}</span>)}</div>}
            {m.action && m.action !== "none" && (
              <div className="proposal" style={{ marginTop: 8 }}>
                <span className="badge ghost">proposes: {m.action.replace(/_/g, " ")}</span>
                {m.proposal?.requirement && <span className="chip">{m.proposal.requirement}</span>}
                {m.proposal?.description && <div className="faint" style={{ fontSize: 11.5, marginTop: 4 }}>{m.proposal.description}</div>}
                {m.status === "pending" && m._key && (
                  <div className="row" style={{ marginTop: 6 }}>
                    <button className="btn sm primary" data-testid="chat-apply" onClick={async () => {
                      try { const r = await api.applyChat(m._key, true); setMsgs((x) => x.map((y) => y._key === m._key ? { ...y, ...r } : y)); toast("success", "Applied"); } catch (e) { toast("error", String((e as Error).message)); }
                    }}>Apply</button>
                    <button className="btn sm" onClick={async () => {
                      try { const r = await api.applyChat(m._key, false); setMsgs((x) => x.map((y) => y._key === m._key ? { ...y, ...r } : y)); } catch (e) { toast("error", String((e as Error).message)); }
                    }}>Discard</button>
                  </div>
                )}
                {m.status === "applied" && <div className="faint" style={{ fontSize: 11, marginTop: 4 }}>✓ applied{(m.work_item ?? m.changeset) ? ` · work item ${m.work_item ?? m.changeset}` : ""}</div>}
                {m.status === "discarded" && <div className="faint" style={{ fontSize: 11, marginTop: 4 }}>discarded</div>}
              </div>
            )}
          </div>
        ))}
        {busy && <div className="msg orchestrator"><span className="row"><span className="spin" /> thinking…</span></div>}
      </div>
      <div className="chat-input">
        {(refs.length > 0 || selectedNode) && (
          <div className="row" style={{ flexWrap: "wrap", gap: 4 }}>
            {refs.map((r) => <span key={r} className="chip"><Link size={10} /> {r.split("/")[1]} <X size={10} style={{ cursor: "pointer" }} onClick={() => setRefs(refs.filter((x) => x !== r))} /></span>)}
            {selectedNode && !refs.includes(selectedNode) && (
              <button className="btn sm ghost-btn" onClick={() => { setRefs([...refs, selectedNode]); onClearRef(); }} data-testid="chat-ref">
                <Link size={11} /> Reference {selectedNode.split("/")[1]}
              </button>
            )}
          </div>
        )}
        <div className="row" style={{ alignItems: "flex-end" }}>
          <textarea className="textarea" placeholder="Message the orchestrator…" value={text} data-testid="chat-input"
            onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } }} />
          <button className="btn primary icon" style={{ height: 40, width: 40 }} onClick={send} disabled={busy || !text.trim()} aria-label="Send"><Send size={15} /></button>
        </div>
      </div>
    </div>
  );
}

function Decisions() {
  const [ds, setDs] = useState<Json[]>([]);
  useEffect(() => { api.decisions().then(setDs).catch(() => {}); }, []);
  return (
    <div style={{ padding: 10 }} data-testid="decisions">
      {ds.length === 0 && <div className="empty">No decisions recorded yet.</div>}
      {ds.map((d) => (
        <div className="card" key={d._key} style={{ padding: 12, marginBottom: 8 }}>
          <div className="row" style={{ alignItems: "flex-start" }}><Scale size={14} /><b style={{ flex: 1 }}>{d.title}</b></div>
          <div style={{ marginTop: 6, fontSize: 12.5 }}>{d.choice}</div>
          <div className="faint" style={{ marginTop: 6, fontSize: 11.5 }}>{d.rationale}</div>
          <div className="row" style={{ marginTop: 8, gap: 4, flexWrap: "wrap" }}>
            <span className="chip">{d.made_by}</span>
            {d.justifies?.map((j: string) => <span className="chip mono" key={j}>{localKey(j)}</span>)}
            <span className="spacer" /><span className="faint" style={{ fontSize: 11 }}>{relTime(d.created_at)}</span>
          </div>
        </div>
      ))}
    </div>
  );
}

export function AgentsPanel({ agents, proposals, requirementsTitle, selectedNode, onClearRef }: {
  agents: Json; proposals: Proposal[]; requirementsTitle: (k: string) => string; selectedNode: string | null; onClearRef: () => void;
}) {
  const [tab, setTab] = useState<"proposals" | "chat" | "decisions">("proposals");
  const open = proposals.filter((c) => !["accepted", "rejected"].includes(c.state));
  const done = proposals.filter((c) => ["accepted", "rejected"].includes(c.state)).slice(0, 6);
  const prevOpen = useRef(open.length);
  useEffect(() => { if (open.length > prevOpen.current) setTab("proposals"); prevOpen.current = open.length; }, [open.length]);
  return (
    <aside className="col sidebar right" data-testid="agents-panel">
      <div className="section-head">
        <span className="section-title">Agent team</span>
        {agents?.backend === "recorded"
          ? <span className="badge at_risk" title="Recorded backend: replays the canned doc 08 demo answers — switch to Claude Code in the status bar for real agents">demo replay</span>
          : <span className="chip" title="Agent backend">{agents?.backend === "claude_code" ? "Claude Code" : agents?.backend}{agents?.token_env ? ` · ${agents.token_env}` : ""}</span>}
      </div>
      <Team agents={agents} active={open[0]} />
      <div className="tabs">
        <button className={`tab ${tab === "proposals" ? "active" : ""}`} onClick={() => setTab("proposals")} data-testid="tab-proposals">Proposals <span className="count">{open.length}</span></button>
        <button className={`tab ${tab === "chat" ? "active" : ""}`} onClick={() => setTab("chat")} data-testid="tab-chat">Orchestrator</button>
        <button className={`tab ${tab === "decisions" ? "active" : ""}`} onClick={() => setTab("decisions")} data-testid="tab-decisions">Decisions</button>
      </div>
      {tab === "proposals" && (
        <div className="scroll" style={{ paddingBottom: 10 }}>
          {open.length === 0 && done.length === 0 && <div className="empty">No proposals. Edit a requirement or ask the orchestrator.</div>}
          {open.map((c) => <ProposalCard key={c._key + c.state} cs={c} requirementsTitle={requirementsTitle} />)}
          {done.length > 0 && <div className="section-head"><span className="section-title">Recently closed</span></div>}
          {done.map((c) => <ProposalCard key={c._key + c.state} cs={c} requirementsTitle={requirementsTitle} />)}
        </div>
      )}
      {tab === "chat" && <Chat selectedNode={selectedNode} onClearRef={onClearRef} />}
      {tab === "decisions" && <div className="scroll"><Decisions /></div>}
    </aside>
  );
}

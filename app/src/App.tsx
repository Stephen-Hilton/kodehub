import { useCallback, useEffect, useMemo, useState } from "react";
import "./theme.css";
import { AgentsPanel } from "./components/AgentsPanel";
import { Eye, Logo, X } from "./components/icons";
import { Insights } from "./components/Insights";
import { MissionBanner } from "./components/MissionBanner";
import { NodePanel } from "./components/NodePanel";
import { ReleasesDrawer } from "./components/ReleasesDrawer";
import { RequirementEditor } from "./components/RequirementEditor";
import { RequirementsPanel } from "./components/RequirementsPanel";
import { StatusBar } from "./components/StatusBar";
import { SystemMap } from "./components/SystemMap";
import { TerminalDock } from "./components/TerminalDock";
import { MainSplitters, TerminalSplitter, usePaneLayout } from "./components/Panes";
import { TimeSlider } from "./components/TimeSlider";
import { ToastProvider, useToast } from "./components/Toasts";
import { SignIn } from "./components/SignIn";
import { api, session, type Json, type MapNode } from "./lib/api";
import { relTime } from "./lib/health";
import { useKodehub } from "./lib/useKodehub";

function Shell() {
  const toast = useToast();
  const [at, setAt] = useState<string | null>(null);
  const [compareAt, setCompareAt] = useState<string | null>(null);
  const [compare, setCompare] = useState<{ names: string[] } | null>(null);
  const data = useKodehub(at);
  const [selected, setSelected] = useState<string | null>(null);
  const [selectedReq, setSelectedReq] = useState<string | null>(null);
  const [highlight, setHighlight] = useState<Set<string> | null>(null);
  const [editor, setEditor] = useState<{ key: string | null; edge: boolean } | null>(null);
  const [environment, setEnvironment] = useState("dev");
  const [showTests, setShowTests] = useState(false);
  const [releasesOpen, setReleasesOpen] = useState(false);
  const [terminalOpen, setTerminalOpen] = useState(false);
  const panes = usePaneLayout();
  const [testsRunning, setTestsRunning] = useState(false);
  const [health, setHealth] = useState<Json>(null);

  useEffect(() => { api.health().then(setHealth).catch(() => {}); }, []);

  // requirement path highlight
  useEffect(() => {
    if (!selectedReq) { setHighlight(null); return; }
    api.requirement(selectedReq).then((r) => {
      const p = r.path ?? {};
      const ids = new Set<string>([...(p.satisfiers ?? []), ...(p.beneath ?? []), ...(p.tests ?? [])]);
      // keep containing compounds lit
      const byId = new Map((data.map?.nodes ?? []).map((n) => [n.id, n]));
      for (const id of [...ids]) {
        let n = byId.get(id);
        while (n?.parent) { ids.add(n.parent); n = byId.get(n.parent); }
      }
      setHighlight(ids);
    }).catch(() => setHighlight(null));
  }, [selectedReq, data.map, data.requirements]);

  // compare two points in time
  useEffect(() => {
    if (!at || !compareAt) { setCompare(null); return; }
    Promise.all([api.map(at), api.map(compareAt)]).then(([a, b]) => {
      const va = new Map(a.nodes.filter((n) => n.level === "symbol").map((n) => [n.id, n.version_at]));
      const names = b.nodes.filter((n) => n.level === "symbol" && va.get(n.id) !== n.version_at).map((n) => n.label);
      setCompare({ names });
    });
  }, [at, compareAt]);

  const selectNode = useCallback((id: string | null, node?: MapNode) => {
    if (!id) { setSelected(null); return; }
    if (node?.ghost) { toast("info", `${node.label} is proposed in changeset ${node.changeset} — it becomes real when that merges.`); return; }
    setSelected(id);
  }, [toast]);

  // keyboard shortcuts
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); document.getElementById("map-search")?.focus(); }
      if (e.ctrlKey && e.key === "`") { e.preventDefault(); setTerminalOpen((t) => !t); }
      if (e.key === "Escape" && !editor && !releasesOpen) { setSelected(null); setSelectedReq(null); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [editor, releasesOpen]);

  const reqTitle = useCallback((k: string) => data.requirements.find((r) => r._key === k)?.title?.split("—")[0].trim() ?? k, [data.requirements]);
  const selectedNode = selected && data.map?.nodes.find((n) => n.id === selected);
  const symbolKey = selectedNode && selectedNode.level === "symbol" && !at ? selectedNode.key : null;
  const testsTotal = useMemo(() => (data.map?.nodes ?? []).filter((n) => n.kind === "test" && !n.ghost).length, [data.map]);
  const atLabel = data.timeline.find((e) => e.at === at);

  const runAll = async () => {
    setTestsRunning(true);
    try {
      const r = await api.runTests();
      toast(r.summary.failed ? "error" : "success", `${r.summary.passed}/${r.summary.total} tests pass (${r.summary.cached} cached)`);
    } catch (e) { toast("error", String((e as Error).message)); } finally { setTestsRunning(false); }
  };

  return (
    <div className="app" style={{ ...panes.appStyle, gridTemplateRows: terminalOpen ? "auto minmax(0,1fr) auto auto auto" : undefined }}>
      <MissionBanner mission={data.map?.mission} requirements={data.requirements} connected={data.connected}
        openChangesets={data.changesets.filter((c) => !["merged", "rejected"].includes(c.state)).length} testsTotal={testsTotal} />
      <div className={`main ${symbolKey ? "with-panel" : ""}`} ref={panes.mainRef} style={panes.mainStyle}>
        <RequirementsPanel
          requirements={data.requirements} groups={data.groups} selected={selectedReq}
          onSelect={setSelectedReq} onOpen={(k) => setEditor({ key: k, edge: false })}
          onAddEdgeCase={(k) => setEditor({ key: k, edge: true })} onNew={() => setEditor({ key: null, edge: false })}
          onRunTests={runAll} running={testsRunning}
          insights={<Insights onSelect={(id) => setSelected(id)} />}
        />
        <div className="col" style={{ position: "relative", minWidth: 0 }}>
          <SystemMap data={data.map} selected={selected} highlight={highlight} onSelect={selectNode}
            showTests={showTests} setShowTests={setShowTests} historyMode={!!at}>
            {at && (
              <div className="history-banner glass" data-testid="history-banner">
                <Eye size={14} />
                <span>Viewing the system as of <b>{atLabel?.label ?? at}</b> · {relTime(at)}</span>
                {compare && <span className="badge ghost">{compare.names.length} changed between points{compare.names.length ? `: ${compare.names.slice(0, 4).join(", ")}${compare.names.length > 4 ? "…" : ""}` : ""}</span>}
                <button className="btn sm" onClick={() => { setAt(null); setCompareAt(null); }}>Back to live</button>
              </div>
            )}
            {selectedReq && !at && (
              <div className="req-highlight-banner glass">
                <span className="dot" style={{ background: "var(--accent)" }} />
                Path of <b>{reqTitle(selectedReq)}</b>
                <button className="btn sm icon ghost-btn" aria-label="Clear highlight" onClick={() => setSelectedReq(null)}><X size={12} /></button>
              </div>
            )}
          </SystemMap>
          {data.error && !data.map && <div className="empty" style={{ position: "absolute", top: "40%", left: 0, right: 0 }}>Cannot reach kodehub-server: {data.error}</div>}
          {data.map && data.map.nodes.length === 0 && !data.loading && (
            <div className="welcome glass" data-testid="welcome">
              <Logo size={44} />
              <h2>Welcome to Kodehub</h2>
              <p className="muted">Code lives in a graph, connected to the requirements it satisfies, the tests that verify it and the decisions behind it. Start from the proof-of-concept project, or write your first requirement.</p>
              <div className="row" style={{ justifyContent: "center", marginTop: 14 }}>
                <button className="btn primary" data-testid="load-example" onClick={async () => {
                  try { await api.seed("examples/auth-poc"); toast("success", "Loaded the auth-poc example"); data.refresh(); } catch (e) { toast("error", String((e as Error).message)); }
                }}>Load the auth example</button>
                <button className="btn" onClick={() => setEditor({ key: null, edge: false })}>Write a requirement</button>
              </div>
            </div>
          )}
        </div>
        {symbolKey && (
          <NodePanel symbolKey={symbolKey} environment={environment} onClose={() => setSelected(null)}
            onNavigate={(id) => setSelected(id)} onOpenRequirement={(k) => setEditor({ key: k, edge: false })}
            requirements={data.requirements} />
        )}
        <AgentsPanel agents={data.agents} changesets={data.changesets} requirementsTitle={reqTitle} selectedNode={selected} onClearRef={() => {}} />
        <MainSplitters mainRef={panes.mainRef} panel={!!symbolKey} setPane={panes.setPane} />
      </div>
      <TimeSlider events={data.timeline} at={at} compareAt={compareAt}
        onPick={(t, cmp) => { if (cmp && at) setCompareAt(t); else { setAt(t); setCompareAt(null); setSelected(null); } }}
        onLive={() => { setAt(null); setCompareAt(null); }} />
      <StatusBar environment={environment} setEnvironment={setEnvironment} environments={data.environments} settings={data.settings} health={health}
        onSetToken={(t) => api.patchAgents({ token_env: t }).then(() => toast("success", t ? `New agent work will use ${t}` : "Agents will use the claude CLI login")).catch((e) => toast("error", e.message))}
        onSetBackend={(b) => api.patchAgents({ backend: b }).then(() => toast("success", `Agent backend: ${b}`)).catch((e) => toast("error", e.message))}
        onReleases={() => setReleasesOpen(true)} onTerminal={() => setTerminalOpen((t) => !t)} terminalOpen={terminalOpen} />
      {terminalOpen && <TerminalDock onClose={() => setTerminalOpen(false)} resizer={<TerminalSplitter setPane={panes.setPane} />} />}
      {editor && editor.key && editor.key !== selectedReq && <ReqHighlight k={editor.key} set={setSelectedReq} />}
      {editor && <RequirementEditor reqKey={editor.key} groups={data.groups} focusEdgeCase={editor.edge} onClose={() => setEditor(null)} />}
      {releasesOpen && <ReleasesDrawer releases={data.releases} environments={data.environments} onClose={() => setReleasesOpen(false)} />}
    </div>
  );
}

/** While a requirement is open in the editor, light its path on the map. */
function ReqHighlight({ k, set }: { k: string; set: (k: string) => void }) {
  useEffect(() => { set(k); }, [k, set]);
  return null;
}

/** Shows the sign-in screen until a valid session exists, then the app. */
function AuthGate() {
  const [state, setState] = useState<"checking" | "signed-out" | "ready">("checking");
  const check = useCallback(() => {
    if (!session.token()) { setState("signed-out"); return; }
    api.me()
      .then(async () => {
        if (!session.project()) {
          const ps = await api.projects().catch(() => []);
          if (ps.length) session.setProject(ps[0].id);
        }
        setState("ready");
      })
      .catch(() => setState("signed-out"));
  }, []);
  useEffect(() => {
    check();
    const out = () => setState("signed-out");
    window.addEventListener("kodehub:signed-out", out);
    return () => window.removeEventListener("kodehub:signed-out", out);
  }, [check]);
  if (state === "checking") return null;
  if (state === "signed-out") return <SignIn onSignedIn={check} />;
  return <Shell />;
}

export function App() {
  return (
    <ToastProvider>
      <AuthGate />
    </ToastProvider>
  );
}

import type { Core } from "cytoscape";
import { useCallback, useEffect, useMemo, useState } from "react";
import "./theme.css";
import { Eye, Logo, X } from "./components/icons";
import { MissionBanner } from "./components/MissionBanner";
import { NodeBadges, type BadgeClick } from "./components/NodeBadges";
import { NodeOverlay, Popover } from "./components/NodeOverlays";
import { NodePanel } from "./components/NodePanel";
import { ReleasesDrawer } from "./components/ReleasesDrawer";
import { RequirementEditor } from "./components/RequirementEditor";
import { SignIn } from "./components/SignIn";
import { StatusBar } from "./components/StatusBar";
import { SystemMap } from "./components/SystemMap";
import { TerminalDock } from "./components/TerminalDock";
import { TimeSlider } from "./components/TimeSlider";
import { ToastProvider, useToast } from "./components/Toasts";
import { api, session, type Json, type MapNode } from "./lib/api";
import { relTime } from "./lib/health";
import { useKodehub } from "./lib/useKodehub";
import { ProjectsScreen } from "./screens/ProjectsScreen";
import { RequirementsScreen } from "./screens/RequirementsScreen";
import { SettingsScreen } from "./screens/SettingsScreen";
import { UseCasesScreen } from "./screens/UseCasesScreen";
import { WorkScreen } from "./screens/WorkScreen";

type Screen = "map" | "requirements" | "use_cases" | "work" | "releases" | "settings" | "projects";
const TABS: { id: Screen; label: string }[] = [
  { id: "map", label: "Map" },
  { id: "requirements", label: "Requirements" },
  { id: "use_cases", label: "Use cases" },
  { id: "work", label: "Work" },
  { id: "releases", label: "Releases" },
  { id: "settings", label: "Settings" },
];

/** Project switcher, screens and the signed-in user (spec §8.4). */
function TopNav({ screen, setScreen, me, projects, project, onProject, attention }: {
  screen: Screen; setScreen: (s: Screen) => void; me: Json; projects: Json[]; project: string | null; onProject: (id: string) => void; attention: number;
}) {
  return (
    <nav className="topnav" data-testid="topnav">
      <select className="input project-switch" value={project ?? ""} onChange={(e) => (e.target.value === "__all" ? setScreen("projects") : onProject(e.target.value))}
        data-testid="project-switch" aria-label="Project">
        {projects.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
        <option value="__all">All projects…</option>
      </select>
      <div className="tabs">
        {TABS.map((t) => (
          <button key={t.id} className={`tab ${screen === t.id ? "on" : ""}`} onClick={() => setScreen(t.id)} data-testid={`tab-${t.id}`}>
            {t.label}{t.id === "work" && attention > 0 && <span className="tab-count">{attention}</span>}
          </button>
        ))}
      </div>
      <span className="spacer" />
      <span className="faint" data-testid="me">{me?.user?.display_name}</span>
      <button className="btn sm ghost-btn" data-testid="sign-out" onClick={() => api.logout().finally(() => { session.set(null); session.setProject(null); window.location.reload(); })}>Sign out</button>
    </nav>
  );
}

function Shell({ me }: { me: Json }) {
  const toast = useToast();
  const [project, setProject] = useState<string | null>(session.project());
  const [projects, setProjects] = useState<Json[]>([]);
  const [screen, setScreen] = useState<Screen>(session.project() ? "map" : "projects");
  const [at, setAt] = useState<string | null>(null);
  const [compareAt, setCompareAt] = useState<string | null>(null);
  const [compare, setCompare] = useState<{ names: string[] } | null>(null);
  const data = useKodehub(at);
  const [selected, setSelected] = useState<{ id: string; x: number; y: number } | null>(null);
  const [highlight, setHighlight] = useState<Set<string> | null>(null);
  const [selectedReq, setSelectedReq] = useState<string | null>(null);
  const [editor, setEditor] = useState<{ key: string | null; edge: boolean } | null>(null);
  const [environment, setEnvironment] = useState("dev");
  const [showTests, setShowTests] = useState(false);
  const [terminalOpen, setTerminalOpen] = useState(false);
  const [testsRunning, setTestsRunning] = useState(false);
  const [health, setHealth] = useState<Json>(null);
  const [cy, setCy] = useState<Core | null>(null);
  const [badge, setBadge] = useState<BadgeClick | null>(null);
  const [attention, setAttention] = useState(0);

  const loadProjects = useCallback(() => api.projects().then(setProjects).catch(() => {}), []);
  useEffect(() => { loadProjects(); api.health().then(setHealth).catch(() => {}); }, [loadProjects]);
  useEffect(() => {
    const poll = () => Promise.all([api.approvals().catch(() => []), api.notifications().catch(() => [])])
      .then(([a, n]) => setAttention(a.length + n.length));
    poll();
    const t = window.setInterval(poll, 8000);
    return () => window.clearInterval(t);
  }, [project]);

  const openProject = (id: string) => {
    session.setProject(id);
    api.openProject(id).catch(() => {});
    setProject(id);
    // every view re-reads for the new project
    window.location.reload();
  };

  // requirement path highlight
  useEffect(() => {
    if (!selectedReq) { setHighlight(null); return; }
    api.requirement(selectedReq).then((r) => {
      const p = r.path ?? {};
      const ids = new Set<string>([...(p.applies_to ?? []), ...(p.beneath ?? []), ...(p.tests ?? [])]);
      const byId = new Map((data.map?.nodes ?? []).map((n) => [n.id, n]));
      for (const id of [...ids]) {
        let n = byId.get(id);
        while (n?.parent) { ids.add(n.parent); n = byId.get(n.parent); }
      }
      setHighlight(ids);
    }).catch(() => setHighlight(null));
  }, [selectedReq, data.map, data.requirements]);

  useEffect(() => {
    if (!at || !compareAt) { setCompare(null); return; }
    Promise.all([api.map(at), api.map(compareAt)]).then(([a, b]) => {
      // the version of each code node in either project version (the live map has no version_at)
      const ver = (n: MapNode) => n.version_at ?? n.version ?? null;
      const va = new Map(a.nodes.filter((n) => n.level === "symbol" && !n.ghost).map((n) => [n.id, ver(n)]));
      const names = b.nodes.filter((n) => n.level === "symbol" && !n.ghost && va.get(n.id) !== ver(n)).map((n) => n.label);
      setCompare({ names });
    });
  }, [at, compareAt]);

  /** Anchor point (viewport) next to a node on the map. */
  const anchorOf = useCallback((id: string) => {
    const n = cy?.getElementById(id);
    const box = cy?.container()?.getBoundingClientRect();
    if (!n || n.empty() || !box) return { x: window.innerWidth / 2, y: 120 };
    const bb = n.renderedBoundingBox({});
    return { x: box.left + bb.x2 + 10, y: box.top + Math.max(bb.y1, 0) };
  }, [cy]);

  const selectNode = useCallback((id: string | null, node?: MapNode) => {
    setBadge(null);
    if (!id) { setSelected(null); return; }
    if (node?.ghost) { toast("info", `${node.label} is proposed by work item ${node.work_item} — it becomes real when its proposal is accepted.`); return; }
    setSelected({ id, ...anchorOf(id) });
  }, [toast, anchorOf]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); setScreen("map"); window.setTimeout(() => document.getElementById("map-search")?.focus(), 0); }
      if (e.ctrlKey && e.key === "`") { e.preventDefault(); setTerminalOpen((t) => !t); }
      if (e.key === "Escape" && !editor) { setSelected(null); setBadge(null); setSelectedReq(null); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [editor]);

  const reqTitle = useCallback((k: string) => data.requirements.find((r) => r._key === k)?.title?.split("—")[0].trim() ?? k, [data.requirements]);
  const selectedNode = selected && data.map?.nodes.find((n) => n.id === selected.id);
  const symbolKey = selectedNode && selectedNode.level === "symbol" && !at ? selectedNode.key : null;
  const testsTotal = useMemo(() => (data.map?.nodes ?? []).filter((n) => n.kind === "test" && !n.ghost).length, [data.map]);
  const tests = useMemo(() => (data.map?.nodes ?? []).filter((n) => n.kind === "test" && !n.ghost).map((n) => ({ id: n.id, label: n.label })), [data.map]);
  // `at` is a project version id; its stop on the time slider names it
  const atLabel = data.timeline.find((e) => e.project_version === at && e.kind !== "accept")
    ?? data.timeline.find((e) => e.project_version === at);

  const runAll = async () => {
    setTestsRunning(true);
    try {
      const r = await api.runTests();
      toast(r.summary.failed ? "error" : "success", `${r.summary.passed}/${r.summary.total} tests pass (${r.summary.cached} cached)`);
    } catch (e) { toast("error", String((e as Error).message)); } finally { setTestsRunning(false); }
  };

  const goToNode = (id: string) => {
    setScreen("map");
    window.setTimeout(() => {
      const n = cy?.getElementById(id);
      if (n && !n.empty()) cy?.animate({ center: { eles: n }, zoom: Math.max(cy.zoom(), 1) } as never, { duration: 250 });
      setSelected({ id, ...anchorOf(id) });
    }, 50);
  };

  return (
    <div className="app v01" style={{ gridTemplateRows: terminalOpen ? "auto auto minmax(0,1fr) auto auto auto" : undefined }}>
      <MissionBanner mission={data.map?.mission} requirements={data.requirements} connected={data.connected}
        openChangesets={data.proposals.filter((c) => !["accepted", "rejected"].includes(c.state)).length} testsTotal={testsTotal} />
      <TopNav screen={screen} setScreen={(s) => { setScreen(s); setBadge(null); setSelected(null); }} me={me} projects={projects}
        project={project} onProject={openProject} attention={attention} />
      <div className="main single">
        {/* the map stays mounted (and keeps its layout) while other screens are shown */}
        {(
          <div className="col" style={{ position: "relative", minWidth: 0, display: screen === "map" ? "flex" : "none" }}>
            <SystemMap data={data.map} selected={selected?.id ?? null} highlight={highlight} onSelect={selectNode}
              showTests={showTests} setShowTests={setShowTests} historyMode={!!at} onCy={setCy}>
              <NodeBadges cy={cy} onOpen={(b) => { setSelected(null); setBadge(b); }} />
              {at && (
                <div className="history-banner glass" data-testid="history-banner">
                  <Eye size={14} />
                  <span>Viewing project version <b>{data.map?.project_version?.name ?? atLabel?.label ?? at}</b>{atLabel?.at ? <> · {relTime(atLabel.at)}</> : null}</span>
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
                <h2>Nothing built yet</h2>
                <p className="muted">Agents plan contexts into containers and components, write tests first, then the code. Start from the proof-of-concept project, write a requirement, or import a repository in Settings.</p>
                <div className="row" style={{ justifyContent: "center", marginTop: 14 }}>
                  <button className="btn primary" data-testid="load-example" onClick={async () => {
                    try { await api.seed("examples/auth-poc"); toast("success", "Loaded the auth-poc example"); data.refresh(); } catch (e) { toast("error", String((e as Error).message)); }
                  }}>Load the auth example</button>
                  <button className="btn" onClick={() => setEditor({ key: null, edge: false })}>Write a requirement</button>
                </div>
              </div>
            )}
            {badge && screen === "map" && <NodeOverlay node={badge.node} kind={badge.kind} x={badge.x} y={badge.y} onClose={() => setBadge(null)}
              onOpenRequirement={(k) => setEditor({ key: k, edge: false })} />}
            {symbolKey && selected && screen === "map" && (
              <Popover x={selected.x} y={selected.y} width={460} onClose={() => setSelected(null)} testid="node-overlay">
                <NodePanel symbolKey={symbolKey} environment={environment} onClose={() => setSelected(null)}
                  onNavigate={(id) => goToNode(id)} onOpenRequirement={(k) => setEditor({ key: k, edge: false })} requirements={data.requirements} />
              </Popover>
            )}
          </div>
        )}
        {screen === "requirements" && (
          <RequirementsScreen requirements={data.requirements} tags={data.tags} onOpen={(k) => setEditor({ key: k, edge: false })}
            onAddEdgeCase={(k) => setEditor({ key: k, edge: true })} onNew={() => setEditor({ key: null, edge: false })}
            onRunTests={runAll} running={testsRunning} onSelectNode={goToNode}
            selected={selectedReq} setSelected={setSelectedReq} />
        )}
        {screen === "use_cases" && <UseCasesScreen requirements={data.requirements} tests={tests} />}
        {screen === "work" && <WorkScreen agents={data.agents} proposals={data.proposals} reqTitle={reqTitle} />}
        {screen === "releases" && <ReleasesDrawer inline versions={data.versions} environments={data.environments} onClose={() => setScreen("map")} />}
        {screen === "settings" && project && <SettingsScreen project={project} me={me} />}
        {screen === "projects" && <ProjectsScreen current={project} onOpen={openProject} />}
      </div>
      <TimeSlider events={data.timeline} at={at} compareAt={compareAt}
        onPick={(t, cmp) => {
          const current = data.versions.find((v) => v.current)?._id;
          if (cmp && at) setCompareAt(t);
          else { setAt(t === current ? null : t); setCompareAt(null); setSelected(null); setScreen("map"); }
        }}
        onLive={() => { setAt(null); setCompareAt(null); }} />
      <StatusBar environment={environment} setEnvironment={setEnvironment} environments={data.environments} settings={data.settings} health={health}
        onSetToken={(t) => api.patchAgents({ token_env: t }).then(() => toast("success", t ? `New agent work will use ${t}` : "Agents will use the claude CLI login")).catch((e) => toast("error", e.message))}
        onSetBackend={(b) => api.patchAgents({ backend: b }).then(() => toast("success", `Agent backend: ${b}`)).catch((e) => toast("error", e.message))}
        onReleases={() => setScreen("releases")} onTerminal={() => setTerminalOpen((t) => !t)} terminalOpen={terminalOpen} />
      {terminalOpen && <TerminalDock onClose={() => setTerminalOpen(false)} resizer={null} />}
      {editor && editor.key && editor.key !== selectedReq && <ReqHighlight k={editor.key} set={setSelectedReq} />}
      {editor && <RequirementEditor reqKey={editor.key} tags={data.tags} focusEdgeCase={editor.edge} onClose={() => setEditor(null)} />}
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
  const [me, setMe] = useState<Json>(null);
  const check = useCallback(() => {
    if (!session.token()) { setState("signed-out"); return; }
    api.me()
      .then(async (m) => {
        setMe(m);
        const ps = await api.projects().catch(() => []);
        const cur = session.project();
        if (!cur || !ps.some((p: Json) => p.id === cur)) session.setProject(ps.length ? ps[0].id : null);
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
  return <Shell me={me} />;
}

export function App() {
  return (
    <ToastProvider>
      <AuthGate />
    </ToastProvider>
  );
}

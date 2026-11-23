import type { Json, Requirement } from "../lib/api";
import { Logo } from "./icons";

export function MissionBanner({ mission, requirements, openChangesets, connected, testsTotal }: {
  mission: Json; requirements: Requirement[]; openChangesets: number; connected: boolean; testsTotal: number;
}) {
  const passing = requirements.filter((r) => r.status === "pass").length;
  const failing = requirements.filter((r) => r.status === "fail").length;
  const risk = requirements.filter((r) => r.status === "at_risk" || r.status === "uncovered").length;
  return (
    <header className="banner" data-testid="mission-banner" data-tauri-drag-region>
      <div className="brand">
        <Logo />
        <div>
          Kodehub
          <small>mission control</small>
        </div>
      </div>
      <div className="mission">
        <span className="mission-tag">Mission</span>
        <span className="mission-text" title={mission?.statement}>{mission?.statement ?? "No mission yet"}</span>
      </div>
      <div className="kpis">
        <div className="kpi" title="Requirements whose tests all pass at current closure hashes">
          <span className="dot" style={{ background: "var(--pass)" }} />
          <b data-testid="kpi-passing">{passing}/{requirements.length}</b> passing
        </div>
        {risk > 0 && (
          <div className="kpi"><span className="dot" style={{ background: "var(--risk)" }} /><b>{risk}</b> at risk</div>
        )}
        {failing > 0 && (
          <div className="kpi"><span className="dot" style={{ background: "var(--fail)" }} /><b>{failing}</b> failing</div>
        )}
        <div className="kpi"><b>{testsTotal}</b> tests</div>
        <div className="kpi"><b>{openChangesets}</b> open changesets</div>
      </div>
      <div className="live" title={connected ? "Live updates connected" : "Reconnecting…"}>
        <span className="dot pulse" style={{ background: connected ? "var(--pass)" : "var(--fail)" }} />
        {connected ? "Live" : "Offline"}
      </div>
    </header>
  );
}

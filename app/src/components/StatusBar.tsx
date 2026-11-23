import type { Json } from "../lib/api";
import { Bot, Rocket, TerminalIcon } from "./icons";

export function StatusBar({ environment, setEnvironment, environments, settings, onSetToken, onSetBackend, onReleases, onTerminal, terminalOpen, health }: {
  environment: string; setEnvironment: (e: string) => void; environments: Json[]; settings: Json;
  onSetToken: (name: string) => void; onSetBackend: (b: string) => void; onReleases: () => void; onTerminal: () => void; terminalOpen: boolean; health: Json;
}) {
  const ag = settings?.agents ?? {};
  const vars: string[] = ag.available_token_vars ?? [];
  return (
    <footer className="statusbar" data-testid="status-bar">
      <div className="status-item">
        <span className="faint">Environment</span>
        <select className="select" value={environment} onChange={(e) => setEnvironment(e.target.value)} data-testid="env-select">
          {(environments.length ? environments : [{ _key: "dev" }]).map((e: Json) => (
            <option key={e._key} value={e._key}>
              {e._key}{e._key === "dev" ? " · working state" : e.currently_running ? ` · ${e.currently_running.name}` : " · nothing deployed"}
            </option>
          ))}
        </select>
      </div>
      <div className="status-item" title="Which agent backend runs new work">
        <Bot size={13} />
        <select className="select" value={ag.backend ?? "claude_code"} onChange={(e) => onSetBackend(e.target.value)} data-testid="backend-select">
          <option value="claude_code">Claude Code</option>
          <option value="api">Messages API{ag.api_key_configured ? "" : " (no key)"}</option>
          <option value="recorded">Recorded (demo)</option>
        </select>
      </div>
      {(ag.backend ?? "claude_code") === "claude_code" && (
        <div className="status-item" title="Long-lived Claude Code token for headless agents — Kodehub stores only the variable name">
          <span className="faint">Agent login</span>
          <select className="select" value={ag.token_env ?? ""} onChange={(e) => onSetToken(e.target.value)} data-testid="token-select">
            <option value="">claude login (default)</option>
            {vars.map((v) => <option key={v} value={v}>{v}</option>)}
          </select>
        </div>
      )}
      <span className="faint mono" style={{ fontSize: 11 }}>{ag.model}</span>
      <span className="spacer" />
      <span className="faint" style={{ fontSize: 11.5 }}>ArangoDB {health?.db?.version ?? "…"} · {health?.db?.database}</span>
      <button className="btn sm" onClick={onReleases} data-testid="open-releases"><Rocket size={12} /> Releases</button>
      <button className={`btn sm ${terminalOpen ? "primary" : ""}`} onClick={onTerminal} data-testid="toggle-terminal" title="Terminal (⌃`)"><TerminalIcon size={12} /> Terminal</button>
    </footer>
  );
}

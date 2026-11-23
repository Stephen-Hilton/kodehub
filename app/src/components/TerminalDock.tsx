import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { api, uiTokenQuery, wsUrl } from "../lib/api";
import { Link, TerminalIcon, X } from "./icons";
import { useToast } from "./Toasts";

/** Dockable terminal: a server-side PTY bridged over WebSocket (doc 05). */
export function TerminalDock({ onClose, resizer }: { onClose: () => void; resizer?: ReactNode }) {
  const toast = useToast();
  const host = useRef<HTMLDivElement>(null);
  const ws = useRef<WebSocket | null>(null);
  const [connected, setConnected] = useState(false);
  const offered = useRef(false);

  const registerMcp = async () => {
    const { command } = await api.mcpCommand();
    ws.current?.send(JSON.stringify({ type: "input", data: command + "\r" }));
  };

  useEffect(() => {
    const term = new Terminal({
      fontFamily: "'SF Mono', 'JetBrains Mono', Menlo, monospace", fontSize: 12.5, cursorBlink: true, allowProposedApi: true,
      theme: { background: "#06080d", foreground: "#d6dcea", cursor: "#8b9bff", selectionBackground: "rgba(139,155,255,0.35)" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host.current!);
    const socket = new WebSocket(wsUrl("/api/terminal" + uiTokenQuery()));
    ws.current = socket;
    const sendResize = () => {
      try { fit.fit(); } catch { /* not visible */ }
      if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
    };
    socket.onopen = () => { setConnected(true); sendResize(); };
    socket.onclose = () => setConnected(false);
    socket.onmessage = (m) => term.write(typeof m.data === "string" ? m.data : "");
    let line = "";
    term.onData((d) => {
      socket.readyState === WebSocket.OPEN && socket.send(JSON.stringify({ type: "input", data: d }));
      if (d === "\r") {
        if (/^\s*claude(\s|$)/.test(line) && !/mcp add/.test(line) && !offered.current) {
          offered.current = true;
          toast("info", "Claude Code is running here. Let it read and change the graph through the same API as the built-in agents?", {
            label: "Register Kodehub MCP server", run: registerMcp,
          });
        }
        line = "";
      } else if (d === "\u007f") line = line.slice(0, -1);
      else if (d.length === 1 && d >= " ") line += d;
    });
    const ro = new ResizeObserver(sendResize);
    ro.observe(host.current!);
    term.focus();
    (window as unknown as { __kodehubTerm: Terminal }).__kodehubTerm = term;
    return () => { ro.disconnect(); socket.close(); term.dispose(); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="terminal-dock" data-testid="terminal">
      {resizer}
      <div className="terminal-head">
        <TerminalIcon size={13} /> <b style={{ color: "var(--text)" }}>Terminal</b>
        <span className="dot" style={{ background: connected ? "var(--pass)" : "var(--fail)" }} />
        <span className="faint">{connected ? "zsh · ANTHROPIC_API_KEY removed" : "connecting…"}</span>
        <span className="spacer" />
        <button className="btn sm" onClick={registerMcp} title="Register the Kodehub MCP server with Claude Code" data-testid="register-mcp"><Link size={11} /> Connect Claude Code</button>
        <button className="btn sm icon ghost-btn" onClick={onClose} aria-label="Close terminal"><X size={13} /></button>
      </div>
      <div className="terminal-body" ref={host} />
    </div>
  );
}

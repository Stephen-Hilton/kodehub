import cytoscape, { type Core, type StylesheetJson } from "cytoscape";
import fcose from "cytoscape-fcose";
import { useEffect, useMemo, useRef, useState } from "react";
import { localKey, type MapData, type MapNode } from "../lib/api";
import { HEALTH } from "../lib/health";
import { buildElements, computeLayout, depthForZoom, isVisible, LEVELS, nodeWidth, shownOnMap } from "../lib/mapModel";
import { Fit, Layers, Search } from "./icons";

cytoscape.use(fcose);

const GLYPH: Record<string, string> = { function: "ƒ", class: "▣", enum: "◈", trait: "◇", test: "✓" };

function style(): StylesheetJson {
  const s: StylesheetJson = [
    {
      selector: "node",
      style: {
        "font-family": "-apple-system, BlinkMacSystemFont, 'SF Pro Text', Inter, sans-serif",
        color: "#e7eaf3",
        "font-size": 11,
        "text-wrap": "wrap",
        "transition-property": "opacity, border-color, background-color, underlay-opacity",
        "transition-duration": 180,
      },
    },
    {
      selector: "node.context, node.container, node.component",
      style: {
        shape: "round-rectangle",
        "background-color": "#0f1522",
        "background-opacity": 0.55,
        "border-width": 1,
        "border-color": "rgba(148,163,184,0.22)",
        label: "data(label)",
        "text-valign": "top",
        "text-halign": "center",
        "text-margin-y": -8,
        "font-weight": 650,
        "font-size": 15,
        padding: "26px",
        "corner-radius": "14px",
        color: "#c7cee0",
      },
    },
    { selector: "node.context", style: { "background-color": "#0d1220", "border-color": "rgba(139,155,255,0.35)", "font-size": 18, padding: "30px", color: "#b9c3ff" } },
    { selector: "node.container", style: { "background-color": "#111829", "border-style": "dashed", "border-color": "rgba(148,163,184,0.25)" } },
    { selector: "node.component", style: { "background-color": "#141c2e", "border-color": "rgba(148,163,184,0.28)", color: "#d6dcea" } },
    {
      selector: "node.collapsed",
      style: {
        label: (e: cytoscape.NodeSingular) => `${e.data("label")}\n${e.data("kids")} inside · double-click`,
        "text-valign": "center",
        "text-margin-y": 0,
        width: 180,
        height: 70,
        "line-height": 1.5,
      },
    },
    {
      selector: "node.symbol",
      style: {
        shape: "round-rectangle",
        "corner-radius": "9px",
        width: (e: cytoscape.NodeSingular) => nodeWidth(String(e.data("label"))),
        height: 44,
        "background-color": "#121a29",
        "border-width": 1.6,
        "border-color": "#64748b",
        label: (e: cytoscape.NodeSingular) => `${GLYPH[e.data("kind")] ?? "•"}  ${e.data("label")}\n${e.data("sub")}`,
        "text-valign": "center",
        "text-halign": "center",
        "font-family": "'SF Mono', 'JetBrains Mono', Menlo, monospace",
        "font-size": 12.5,
        "line-height": 1.4,
        "text-max-width": "260px",
        "min-zoomed-font-size": 6,
      },
    },
    { selector: "node.symbol.k-test", style: { height: 38, "font-size": 11, "background-color": "#0f1522", color: "#aab3c7" } },
    { selector: "node.symbol.k-class, node.symbol.k-enum, node.symbol.k-trait", style: { "corner-radius": "4px" } },
  ];
  for (const [k, v] of Object.entries(HEALTH)) {
    s.push({ selector: `node.symbol.h-${k}`, style: { "border-color": v.color, "background-color": blend(v.color) } });
    s.push({ selector: `node.component.h-${k}, node.container.h-${k}, node.context.h-${k}`, style: { "border-color": v.color, "border-opacity": 0.55 } as never });
  }
  s.push(
    { selector: "node.symbol.h-pass", style: { "border-color": "#2fbf8a" } },
    { selector: "node.ghost", style: { "border-style": "dashed", "border-width": 2, "border-color": "#a78bfa", "background-color": "#16132a", "background-opacity": 0.7, color: "#d8ccff" } },
    { selector: "node.proposed", style: { "border-style": "dashed", "border-color": "#a78bfa", "border-width": 2.4, "underlay-color": "#a78bfa", "underlay-opacity": 0.18, "underlay-padding": 6, "underlay-shape": "round-rectangle" } as never },
    { selector: "node.symbol.h-uncovered", style: { "border-style": "dotted", "background-color": "#121a29" } },
    { selector: "node.symbol.h-history", style: { "border-color": "#56607a", "background-color": "#121826", color: "#c3c9d8" } },
    { selector: "node.h-history", style: { "border-color": "rgba(148,163,184,0.3)" } },
    { selector: "node.changed", style: { "underlay-color": "#5eead4", "underlay-opacity": 0.25, "underlay-padding": 7, "underlay-shape": "round-rectangle" } as never },
    { selector: "node.sel", style: { "underlay-color": "#8b9bff", "underlay-opacity": 0.35, "underlay-padding": 8, "underlay-shape": "round-rectangle", "border-width": 2.4 } as never },
    { selector: "node.hl", style: { "border-width": 2.4 } },
    { selector: ".dim", style: { opacity: 0.16 } },
    { selector: ".hidden", style: { display: "none" } },
    {
      selector: "edge",
      style: {
        width: 1.3,
        "curve-style": "bezier",
        "line-color": "rgba(139,155,255,0.38)",
        "target-arrow-color": "rgba(139,155,255,0.6)",
        "target-arrow-shape": "triangle",
        "arrow-scale": 0.75,
        opacity: 0.85,
      },
    },
    { selector: "edge.e-depends_on", style: { "line-style": "dashed", "line-color": "rgba(148,163,184,0.35)", "target-arrow-color": "rgba(148,163,184,0.55)" } },
    { selector: "edge.e-tested_with", style: { "line-style": "dotted", "line-color": "rgba(94,234,212,0.35)", "target-arrow-color": "rgba(94,234,212,0.55)" } },
    { selector: "edge.e-binds_to", style: { "line-color": "rgba(251,146,60,0.6)", "target-arrow-color": "rgba(251,146,60,0.8)", width: 2 } },
    { selector: "edge.hl", style: { width: 2.2, opacity: 1 } },
  );
  return s;
}

function blend(hex: string): string {
  // darken a health colour into a tinted node background
  const n = parseInt(hex.slice(1), 16);
  const r = (n >> 16) & 255, g = (n >> 8) & 255, b = n & 255;
  const mix = (c: number, base: number) => Math.round(base + (c - base) * 0.13);
  return `rgb(${mix(r, 18)}, ${mix(g, 26)}, ${mix(b, 41)})`;
}

const DEPTH_LABELS = ["Contexts", "Containers", "Components", "Code"];

export function SystemMap({ data, selected, highlight, onSelect, showTests, setShowTests, historyMode, children, onCy }: {
  data: MapData | null; selected: string | null; highlight: Set<string> | null; onSelect: (id: string | null, node?: MapNode) => void;
  showTests: boolean; setShowTests: (b: boolean) => void; historyMode: boolean; children?: React.ReactNode;
  /** The Cytoscape instance, for layers drawn over the graph (in-node badges). */
  onCy?: (c: Core) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const cy = useRef<Core | null>(null);
  const fitZoom = useRef(1);
  // programmatic fits/pans must not drive semantic zoom (only the user's zooming does)
  const auto = useRef(false);
  const [depthMode, setDepthMode] = useState<number | "auto">("auto");
  const [zoomDepth, setZoomDepth] = useState(4);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [q, setQ] = useState("");
  const [qOpen, setQOpen] = useState(false);
  const [qIdx, setQIdx] = useState(0);
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;
  const onCyRef = useRef(onCy);
  onCyRef.current = onCy;
  const depth = depthMode === "auto" ? zoomDepth : depthMode;

  // create once
  useEffect(() => {
    if (!ref.current) return;
    const c = cytoscape({ container: ref.current, style: style(), wheelSensitivity: 0.25, minZoom: 0.12, maxZoom: 3, boxSelectionEnabled: false });
    cy.current = c;
    c.on("tap", "node", (e) => {
      const id = e.target.id();
      onSelectRef.current(id, e.target.data("__node"));
    });
    c.on("tap", (e) => { if (e.target === c) onSelectRef.current(null); });
    c.on("dbltap", "node", (e) => {
      const id = e.target.id();
      setExpanded((s) => { const n = new Set(s); if (n.has(id)) n.delete(id); else n.add(id); return n; });
    });
    c.on("zoom", () => { if (!auto.current) setZoomDepth(depthForZoom(c.zoom() / (fitZoom.current || 1))); });
    (window as unknown as { __kodehubCy: Core }).__kodehubCy = c;
    onCyRef.current?.(c);
    let fitTimer: number | undefined;
    const ro = new ResizeObserver(() => {
      c.resize();
      // pane resized (node panel docked, terminal opened…): refit if content no longer fits
      window.clearTimeout(fitTimer);
      fitTimer = window.setTimeout(() => {
        const vis = c.nodes(":visible");
        if (vis.empty()) return;
        const bb = vis.boundingBox();
        const ext = c.extent();
        if (bb.x1 < ext.x1 || bb.x2 > ext.x2 || bb.y1 < ext.y1 || bb.y2 > ext.y2) {
          auto.current = true;
          c.animate({ fit: { eles: vis, padding: 40 } } as never, { duration: 250, complete: () => { fitZoom.current = c.zoom(); auto.current = false; } } as never);
        }
      }, 120);
    });
    if (ref.current) ro.observe(ref.current);
    return () => { ro.disconnect(); c.destroy(); };
  }, []);

  const byId = useMemo(() => new Map((data?.nodes ?? []).map((n) => [n.id, n])), [data]);
  const idsKey = useMemo(() => (data?.nodes ?? []).filter((n) => shownOnMap(n, showTests)).map((n) => n.id).sort().join("|") + `|t${showTests}`, [data, showTests]);

  // (re)build elements when the node set changes; otherwise patch classes in place
  const lastIds = useRef("");
  useEffect(() => {
    const c = cy.current;
    if (!c || !data) return;
    const els = buildElements(data, { showTests, highlight, selected });
    if (lastIds.current !== idsKey) {
      const positions = new Map<string, cytoscape.Position>();
      c.nodes().forEach((n) => { positions.set(n.id(), { ...n.position() }); });
      const firstLayout = lastIds.current === "";
      lastIds.current = idsKey;
      c.elements().remove();
      c.add(els.map((e) => ({ ...e, data: { ...e.data, __node: byId.get(e.data.id as string) } })) as never);
      applyVisibility();
      c.nodes().forEach((n) => { const p = positions.get(n.id()); if (p) n.position(p); });
      const pos = computeLayout(data, showTests, Math.max(0.6, c.width() / Math.max(c.height(), 1)));
      const layout = c.layout({
        name: "preset",
        positions: (n: cytoscape.NodeSingular) => pos.get(n.id()) ?? n.position(),
        animate: !firstLayout, animationDuration: 380, fit: false, padding: 40,
      } as never);
      layout.one("layoutstop", () => {
        if (firstLayout) {
          c.fit(undefined, 46);
          fitZoom.current = c.zoom();
          setZoomDepth(4);
          return;
        }
        // new nodes pushed content out of view → refit (keeps the map whole after merges)
        const bb = c.nodes(":visible").boundingBox();
        const ext = c.extent();
        if (bb.x1 < ext.x1 || bb.x2 > ext.x2 || bb.y1 < ext.y1 || bb.y2 > ext.y2) {
          auto.current = true;
          c.animate({ fit: { eles: c.nodes(":visible"), padding: 46 } } as never, {
            duration: 300,
            complete: () => { fitZoom.current = c.zoom(); auto.current = false; },
          } as never);
        }
      });
      layout.run();
    } else {
      c.batch(() => {
        for (const e of els) {
          const el = c.getElementById(e.data.id as string);
          if (!el.nonempty()) continue;
          const hidden = el.hasClass("hidden");
          const collapsed = el.hasClass("collapsed");
          el.data({ ...e.data, __node: byId.get(e.data.id as string) });
          el.classes(e.classes + (hidden ? " hidden" : "") + (collapsed ? " collapsed" : ""));
        }
      });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data, idsKey, highlight, selected]);

  function applyVisibility() {
    const c = cy.current;
    if (!c) return;
    c.batch(() => {
      c.nodes().forEach((n) => {
        const node = byId.get(n.id());
        if (!node) return;
        const vis = isVisible(node, byId, depth, expanded);
        n.toggleClass("hidden", !vis);
        const kids = n.children().toArray();
        const kidVisible = kids.map((k) => { const kn = byId.get(k.id()); return kn ? isVisible(kn, byId, depth, expanded) : false; });
        n.toggleClass("collapsed", vis && kids.length > 0 && kidVisible.every((v) => !v));
      });
    });
  }

  useEffect(() => {
    applyVisibility();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [depth, expanded, idsKey]);

  // centre on selection
  useEffect(() => {
    const c = cy.current;
    if (!c || !selected) return;
    const n = c.getElementById(selected);
    if (n.nonempty() && !n.hasClass("hidden") && n.data("level") === "symbol") {
      // the node panel docks beside the map (the canvas resizes): re-centre once it has
      window.setTimeout(() => {
        c.resize();
        const rp = n.renderedPosition();
        if (rp.x > c.width() - 80 || rp.x < 80 || rp.y < 70 || rp.y > c.height() - 50) {
          const z = c.zoom();
          const p = n.position();
          c.animate({ pan: { x: c.width() / 2 - p.x * z, y: c.height() / 2 - p.y * z } } as never, { duration: 300 });
        }
      }, 60);
    }
  }, [selected]);

  const matches = useMemo(() => {
    const t = q.trim().toLowerCase();
    if (!t || !data) return [];
    return data.nodes.filter((n) => n.label.toLowerCase().includes(t)).slice(0, 12);
  }, [q, data]);

  const choose = (n: MapNode) => {
    setQ(""); setQOpen(false);
    // make sure it's visible
    let p = n.parent ? byId.get(n.parent) : undefined;
    const ex = new Set(expanded);
    while (p) { ex.add(p.id); p = p.parent ? byId.get(p.parent) : undefined; }
    setExpanded(ex);
    if (n.kind === "test" && !showTests) setShowTests(true);
    onSelect(n.id, n);
    const c = cy.current;
    window.setTimeout(() => {
      const el = c?.getElementById(n.id);
      if (!c || !el?.nonempty()) return;
      const z = Math.max(c.zoom(), fitZoom.current);
      const visibleW = c.width();
      const p = el.position();
      c.animate({ zoom: z, pan: { x: visibleW / 2 - p.x * z, y: c.height() / 2 - p.y * z } } as never, { duration: 350 });
    }, 60);
  };

  const setDepth = (d: number | "auto") => {
    setDepthMode(d);
    setExpanded(new Set());
    const c = cy.current;
    if (c && d !== "auto") window.setTimeout(() => c.animate({ fit: { eles: c.nodes(":visible"), padding: 50 } } as never, { duration: 300 }), 30);
    if (c && d === "auto") window.setTimeout(() => { c.fit(undefined, 50); fitZoom.current = c.zoom(); setZoomDepth(4); }, 30);
  };

  return (
    <div className="map-wrap" data-testid="system-map">
      <div className="map-grid" />
      <div className="map-canvas" ref={ref} />
      <div className="map-toolbar">
        <div className="glass search" style={{ position: "relative" }}>
          <Search size={14} />
          <input
            placeholder="Find a node…  ⌘K"
            value={q}
            id="map-search"
            data-testid="map-search"
            onChange={(e) => { setQ(e.target.value); setQOpen(true); setQIdx(0); }}
            onFocus={() => setQOpen(true)}
            onBlur={() => window.setTimeout(() => setQOpen(false), 150)}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") { setQIdx((i) => Math.min(i + 1, matches.length - 1)); e.preventDefault(); }
              if (e.key === "ArrowUp") { setQIdx((i) => Math.max(i - 1, 0)); e.preventDefault(); }
              if (e.key === "Enter" && matches[qIdx]) choose(matches[qIdx]);
              if (e.key === "Escape") { setQ(""); (e.target as HTMLInputElement).blur(); }
            }}
          />
          {qOpen && matches.length > 0 && (
            <div className="search-results glass">
              {matches.map((m, i) => (
                <div key={m.id} className={`search-item ${i === qIdx ? "on" : ""}`} onMouseDown={() => choose(m)} data-testid={`search-result-${localKey(m.key)}`}>
                  <span className="dot" style={{ background: HEALTH[m.health]?.color ?? "#64748b" }} />
                  <span className="mono" style={{ flex: 1 }}>{m.label}</span>
                  <span className="faint" style={{ fontSize: 11 }}>{m.level === "symbol" ? `${m.kind} · ${m.language}` : m.level}</span>
                </div>
              ))}
            </div>
          )}
        </div>
        <div className="glass seg" role="group" aria-label="Zoom level">
          <button className={depthMode === "auto" ? "on" : ""} onClick={() => setDepth("auto")} title="Semantic zoom follows the zoom level"><Layers size={12} /> Auto</button>
          {DEPTH_LABELS.map((l, i) => (
            <button key={l} className={depthMode === i + 1 ? "on" : ""} onClick={() => setDepth(i + 1)} data-testid={`depth-${LEVELS[i]}`} title={l}>
              <span className="lbl-long">{l}</span><span className="lbl-short">{["Ctx", "Ctr", "Cmp", "Code"][i]}</span>
            </button>
          ))}
        </div>
        <div className="glass seg">
          <button className={showTests ? "on" : ""} onClick={() => setShowTests(!showTests)} data-testid="toggle-tests" title="Show test symbols and planted fixtures">Tests</button>
        </div>
        <span className="spacer" />
        {depthMode === "auto" && depth < 4 && <div className="glass seg"><button className="on" style={{ cursor: "default" }} title="Semantic zoom level">Showing {DEPTH_LABELS[depth - 1].toLowerCase()}</button></div>}
      </div>
      {children}
      <div className="map-legend glass">
        <span><i className="dot" style={{ background: "var(--pass)" }} />passing</span>
        <span><i className="dot" style={{ background: "var(--risk)" }} />at risk</span>
        <span><i className="untested-dot" />untested</span>
        <span><i className="dot" style={{ background: "var(--fail)" }} />failing</span>
        <span><i className="dot" style={{ background: "var(--orphan)" }} />orphan</span>
        <span><i className="ghost-dot" />new · Δ changed</span>
        {historyMode && <span><i className="dot" style={{ background: "var(--teal)" }} />changed since then</span>}
      </div>
      <div className="map-zoom glass">
        <button aria-label="Zoom in" onClick={() => cy.current?.zoom({ level: cy.current.zoom() * 1.25, renderedPosition: { x: cy.current.width() / 2, y: cy.current.height() / 2 } })}>+</button>
        <button aria-label="Zoom out" onClick={() => cy.current?.zoom({ level: cy.current.zoom() / 1.25, renderedPosition: { x: cy.current.width() / 2, y: cy.current.height() / 2 } })}>−</button>
        <button aria-label="Fit" onClick={() => cy.current?.animate({ fit: { eles: cy.current.nodes(":visible"), padding: 50 } } as never, { duration: 300 })}><Fit size={14} /></button>
      </div>
    </div>
  );
}

// In-node badges (spec §8.3): small, tight elements drawn over each Cytoscape node — tests
// passed/total (rolled up), business/technical requirement counts (own · with children), the
// lifecycle state, work in progress, a candidate marker and the build dot. The graph itself is
// drawn by Cytoscape exactly as before; this is a positioned HTML layer on top that follows
// pan, zoom and layout. Clicking a badge opens an overlay next to the node.
import type { Core, NodeSingular } from "cytoscape";
import { useEffect, useRef, useState } from "react";
import { localId, type MapNode } from "../lib/api";

export type BadgeKind = "tests" | "requirements" | "lifecycle" | "work";

export interface BadgeClick {
  node: MapNode;
  kind: BadgeKind;
  /** Viewport position to anchor the overlay at. */
  x: number;
  y: number;
}

interface Placed {
  id: string;
  node: MapNode;
  left: number;
  top: number;
  width: number;
  compact: boolean;
}

export const LIFECYCLE_LABEL: Record<string, string> = {
  planned: "Planned",
  in_build: "In build",
  ready_for_test: "Ready for test",
  live: "Live",
};

function testTone(t?: { passed: number; failed: number; total: number }): string {
  if (!t || t.total === 0) return "none";
  if (t.failed > 0) return "fail";
  if (t.passed < t.total) return "risk";
  return "pass";
}

export function NodeBadges({ cy, onOpen }: { cy: Core | null; onOpen: (c: BadgeClick) => void }) {
  const [placed, setPlaced] = useState<Placed[]>([]);
  const frame = useRef<number | null>(null);
  useEffect(() => {
    if (!cy) return;
    const update = () => {
      frame.current = null;
      const out: Placed[] = [];
      cy.nodes(":visible").forEach((n: NodeSingular) => {
        const node = n.data("__node") as MapNode | undefined;
        if (!node || node.ghost) return;
        const bb = n.renderedBoundingBox({ includeLabels: false, includeOverlays: false });
        const w = bb.x2 - bb.x1;
        if (w < 70 || bb.y2 < 0 || bb.x2 < 0) return;
        const compound = n.isParent() || node.level !== "symbol";
        out.push({
          id: node.id,
          node,
          left: bb.x1 + 6,
          // inside the top edge of boxes; just above code nodes, so their labels stay readable
          top: compound ? bb.y1 + 4 : bb.y1 - 19,
          width: w - 12,
          compact: w < 150,
        });
      });
      setPlaced(out);
    };
    const schedule = () => {
      if (frame.current == null) frame.current = requestAnimationFrame(update);
    };
    cy.on("render pan zoom position add remove layoutstop", schedule);
    schedule();
    return () => {
      cy.off("render pan zoom position add remove layoutstop", schedule);
      if (frame.current != null) cancelAnimationFrame(frame.current);
    };
  }, [cy]);

  const click = (e: React.MouseEvent, node: MapNode, kind: BadgeKind) => {
    e.stopPropagation();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    onOpen({ node, kind, x: r.left, y: r.bottom + 6 });
  };

  return (
    <div className="badge-layer" aria-hidden={false}>
      {placed.map((p) => {
        const n = p.node;
        const t = n.test_counts?.total;
        const own = n.test_counts?.own;
        const rq = n.requirements;
        const life = n.lifecycle ?? "planned";
        return (
          <div key={p.id} className={`node-badges ${p.compact ? "compact" : ""}`}
            style={{ left: p.left, top: p.top, maxWidth: p.width }} data-testid={`badges-${localId(n.id)}`}>
            <button className={`nb life ${life}`} title={`Lifecycle: ${LIFECYCLE_LABEL[life] ?? life}`}
              onClick={(e) => click(e, n, "lifecycle")} data-testid={`badge-life-${localId(n.id)}`}>
              <i /> {!p.compact && <span>{LIFECYCLE_LABEL[life]}</span>}
            </button>
            {t && t.total > 0 && (
              <button className={`nb tests ${testTone(t)}`} onClick={(e) => click(e, n, "tests")}
                title={`Tests: ${t.passed}/${t.total} pass (own ${own?.passed ?? 0}/${own?.total ?? 0})`} data-testid={`badge-tests-${localId(n.id)}`}>
                {t.passed}/{t.total}
              </button>
            )}
            {rq && (rq.total.business > 0 || rq.total.technical > 0) && (
              <button className="nb reqs" onClick={(e) => click(e, n, "requirements")}
                title={`Business ${rq.own.business} on this · ${rq.total.business} with children — Technical ${rq.own.technical} · ${rq.total.technical}`}
                data-testid={`badge-reqs-${localId(n.id)}`}>
                <span className="k">B</span>{rq.own.business}{n.level !== "symbol" && <small>·{rq.total.business}</small>}
                <span className="k">T</span>{rq.own.technical}{n.level !== "symbol" && <small>·{rq.total.technical}</small>}
              </button>
            )}
            {(n.in_progress ?? 0) > 0 && (
              <button className="nb work" onClick={(e) => click(e, n, "work")} title={`${n.in_progress} work item(s) on this node`}
                data-testid={`badge-work-${localId(n.id)}`}>⚙{n.in_progress}</button>
            )}
            {(n.candidates ?? 0) > 0 && <span className="nb cand" title="A candidate version is waiting for its gates">◆</span>}
            {t && t.total > 0 && (
              <span className={`nb build ${t.built === t.total ? "ok" : t.built > 0 ? "part" : "none"}`}
                title={t.built === t.total ? "Built" : t.built > 0 ? `${t.built}/${t.total} tests built` : "Not built yet"} />
            )}
          </div>
        );
      })}
    </div>
  );
}

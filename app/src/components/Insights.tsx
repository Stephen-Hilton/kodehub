import { useEffect, useState } from "react";
import { api, type Json } from "../lib/api";
import { onKodehubEvent } from "../lib/useKodehub";
import { Link } from "./icons";

/** "Validation becomes a query": orphans and duplicate candidates, live. */
export function Insights({ onSelect }: { onSelect: (id: string) => void }) {
  const [orphans, setOrphans] = useState<Json[]>([]);
  const [dups, setDups] = useState<{ exact: Json[]; near: Json[] }>({ exact: [], near: [] });
  useEffect(() => {
    const load = () => {
      api.orphans().then(setOrphans).catch(() => {});
      api.nearDuplicates(0.6).then(setDups).catch(() => {});
    };
    load();
    let t: number | undefined;
    const off = onKodehubEvent((e) => {
      if (e.type === "node.changed" || e.type === "changeset.updated") { window.clearTimeout(t); t = window.setTimeout(load, 400); }
    });
    return () => { off(); window.clearTimeout(t); };
  }, []);
  return (
    <div className="insights" data-testid="insights">
      <div className="section-head" style={{ paddingTop: 6 }}><span className="section-title">Graph queries</span></div>
      <div className="insight">
        <div className="row"><span className="dot" style={{ background: orphans.length ? "var(--orphan)" : "var(--pass)" }} /><b>Orphans</b><span className="spacer" /><span className="faint">{orphans.length}</span></div>
        <div className="faint" style={{ fontSize: 11.5 }}>Code no requirement reaches</div>
        {orphans.slice(0, 5).map((o) => (
          <div className="link-item" key={o.id} onClick={() => onSelect(o.id)}><Link size={11} /><span className="mono">{o.display_name}</span></div>
        ))}
      </div>
      <div className="insight">
        <div className="row"><span className="dot" style={{ background: dups.exact.length + dups.near.length ? "var(--ghost)" : "var(--pass)" }} /><b>Merge candidates</b><span className="spacer" /><span className="faint">{dups.exact.length + dups.near.length}</span></div>
        <div className="faint" style={{ fontSize: 11.5 }}>Identical or near-identical symbols</div>
        {dups.exact.map((d) => (
          <div className="link-item" key={d.content_hash} onClick={() => onSelect(d.symbols[0].id)}>
            <span className="badge ghost">same</span><span className="mono" style={{ flex: 1 }}>{d.symbols.map((x: Json) => x.display_name).join(" = ")}</span>
          </div>
        ))}
        {dups.near.slice(0, 4).map((d) => (
          <div className="link-item" key={d.a.id + d.b.id} onClick={() => onSelect(d.a.id)}>
            <span className="badge neutral">{Math.round(d.similarity * 100)}%</span><span className="mono" style={{ flex: 1 }}>{d.a.display_name} ≈ {d.b.display_name}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

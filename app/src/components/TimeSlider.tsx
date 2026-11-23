import type { Json } from "../lib/api";
import { relTime } from "../lib/health";
import { Clock } from "./icons";

export function TimeSlider({ events, at, compareAt, onPick, onLive }: {
  events: Json[]; at: string | null; compareAt: string | null; onPick: (at: string, compare: boolean) => void; onLive: () => void;
}) {
  const n = events.length;
  const pos = (i: number) => (n <= 1 ? 2 : 2 + (i / (n - 1)) * 96);
  const activeIdx = at ? events.findIndex((e) => e.at === at) : n - 1;
  return (
    <div className="timebar" data-testid="time-slider">
      <span className="row muted" style={{ fontSize: 12, minWidth: 110 }}><Clock size={13} /> Time</span>
      <div className="track">
        <div className="track-line" />
        <div className="track-fill" style={{ width: `${activeIdx < 0 ? 0 : pos(activeIdx)}%` }} />
        {events.map((e, i) => (n <= 10 || e.kind === "release" || i === n - 1) && (
          <span key={"l" + e.id + e.at} className={`stop-label ${e.at === at ? "on" : ""}`} style={{ left: `${pos(i)}%` }}>
            {e.kind === "release" ? "◆ " : ""}{String(e.label).replace(/^Address /, "").slice(0, 36)}
          </span>
        ))}
        {events.map((e, i) => (
          <div
            key={e.id + e.at}
            className={`stop ${e.kind} ${e.at === at ? "on" : ""} ${e.at === compareAt ? "cmp" : ""}`}
            style={{ left: `${pos(i)}%` }}
            data-testid={`time-stop-${i}`}
            onClick={(ev) => onPick(e.at, ev.shiftKey)}
            title=""
          >
            <span className="tip">{e.kind === "release" ? "◆ " : e.kind === "merge" ? "⇢ " : ""}{e.label} · {relTime(e.at)}<br /><span className="faint">click to view · shift-click to compare</span></span>
          </div>
        ))}
      </div>
      <button className={`btn sm ${at ? "primary" : ""}`} onClick={onLive} data-testid="time-live">{at ? "Back to live" : "● Live"}</button>
    </div>
  );
}

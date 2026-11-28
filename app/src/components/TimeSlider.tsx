import type { Json } from "../lib/api";
import { relTime } from "../lib/health";
import { Clock } from "./icons";

/** Stops are project versions (opened, frozen) and the proposals accepted into them. Picking a
 *  stop shows its project version: a frozen one exactly as it was released, an open one as it
 *  is now (`at` / `compareAt` are project version ids). */
export function TimeSlider({ events, at, compareAt, onPick, onLive }: {
  events: Json[]; at: string | null; compareAt: string | null; onPick: (projectVersion: string, compare: boolean) => void; onLive: () => void;
}) {
  const n = events.length;
  const pos = (i: number) => (n <= 1 ? 2 : 2 + (i / (n - 1)) * 96);
  const lastOf = (pv: string | null) => {
    for (let i = n - 1; i >= 0; i--) if (events[i].project_version === pv) return i;
    return -1;
  };
  const activeIdx = at ? lastOf(at) : n - 1;
  const isOn = (e: Json, i: number) => !!at && e.project_version === at && i === activeIdx;
  return (
    <div className="timebar" data-testid="time-slider">
      <span className="row muted" style={{ fontSize: 12, minWidth: 110 }}><Clock size={13} /> Time</span>
      <div className="track">
        <div className="track-line" />
        <div className="track-fill" style={{ width: `${activeIdx < 0 ? 0 : pos(activeIdx)}%` }} />
        {events.map((e, i) => (n <= 10 || e.kind === "frozen" || i === n - 1) && (
          <span key={"l" + e.id + e.at} className={`stop-label ${isOn(e, i) ? "on" : ""}`} style={{ left: `${pos(i)}%` }}>
            {e.kind === "frozen" ? "◆ " : ""}{String(e.label).replace(/^Address /, "").slice(0, 36)}
          </span>
        ))}
        {events.map((e, i) => (
          <div
            key={e.id + e.at}
            className={`stop ${e.kind} ${isOn(e, i) ? "on" : ""} ${e.project_version === compareAt ? "cmp" : ""}`}
            style={{ left: `${pos(i)}%` }}
            data-testid={`time-stop-${i}`}
            onClick={(ev) => e.project_version && onPick(e.project_version, ev.shiftKey)}
            title=""
          >
            <span className="tip">{e.kind === "frozen" ? "◆ " : e.kind === "accept" ? "⇢ " : ""}{e.label} · {relTime(e.at)}<br /><span className="faint">click to view its project version · shift-click to compare</span></span>
          </div>
        ))}
      </div>
      <button className={`btn sm ${at ? "primary" : ""}`} onClick={onLive} data-testid="time-live">{at ? "Back to live" : "● Live"}</button>
    </div>
  );
}

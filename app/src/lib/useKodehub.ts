// App data: loads every slice the UI shows and refreshes the right ones on server events.
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type Json, type MapData, type Proposal, type ProjectVersion, type Requirement, type TagCoverage } from "./api";
import { useEvents, type KodehubEvent } from "./events";

type Listener = (e: KodehubEvent) => void;
const listeners = new Set<Listener>();
/** Subscribe to raw events (run output streaming etc.). */
export function onKodehubEvent(l: Listener): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

export interface KodehubData {
  map: MapData | null;
  requirements: Requirement[];
  /** Coverage per requirement tag. */
  tags: TagCoverage[];
  summary: Record<string, number>;
  /** Work-item proposals (code changes in flight and done). */
  proposals: Proposal[];
  agents: Json;
  settings: Json;
  timeline: Json[];
  environments: Json[];
  /** Project versions: the releases. */
  versions: ProjectVersion[];
  connected: boolean;
  error: string | null;
  loading: boolean;
}

const SLICES = ["map", "requirements", "proposals", "agents", "settings", "timeline", "environments", "versions"] as const;
type Slice = (typeof SLICES)[number];

export function useKodehub(at: string | null) {
  const [d, setD] = useState<Omit<KodehubData, "connected">>({
    map: null, requirements: [], tags: [], summary: {}, proposals: [], agents: { agents: [] }, settings: null,
    timeline: [], environments: [], versions: [], error: null, loading: true,
  });
  const atRef = useRef(at);
  atRef.current = at;

  const load = useCallback(async (which: readonly Slice[]) => {
    const tasks: Promise<Partial<KodehubData>>[] = which.map(async (s): Promise<Partial<KodehubData>> => {
      switch (s) {
        case "map": return { map: await api.map(atRef.current) };
        case "requirements": {
          const r = await api.requirements();
          return { requirements: r.requirements, tags: r.tags, summary: r.summary };
        }
        case "proposals": return { proposals: await api.proposals() };
        case "agents": return { agents: await api.agents() };
        case "settings": return { settings: await api.settings() };
        case "timeline": return { timeline: (await api.timeline()).events };
        case "environments": return { environments: await api.environments() };
        case "versions": return { versions: await api.projectVersions() };
      }
    });
    const results = await Promise.allSettled(tasks);
    setD((prev) => {
      let next = { ...prev, loading: false };
      let error: string | null = null;
      for (const r of results) {
        if (r.status === "fulfilled") next = { ...next, ...r.value };
        else error = String(r.reason?.message ?? r.reason);
      }
      return { ...next, error };
    });
  }, []);

  useEffect(() => {
    load(SLICES);
  }, [load]);
  useEffect(() => {
    load(["map"]);
  }, [at, load]);

  // debounce refreshes per slice
  const pending = useRef(new Set<Slice>());
  const timer = useRef<number | undefined>(undefined);
  const schedule = useCallback(
    (slices: Slice[]) => {
      slices.forEach((s) => pending.current.add(s));
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => {
        const s = [...pending.current];
        pending.current.clear();
        load(s);
      }, 180);
    },
    [load],
  );

  const connected = useEvents((e) => {
    listeners.forEach((l) => l(e));
    switch (e.type) {
      case "node.changed":
        schedule(["map", "requirements", "timeline", "proposals"]);
        break;
      case "proposal.updated":
        schedule(["proposals", "map", "requirements", "timeline"]);
        break;
      case "test.result":
        schedule(["map", "requirements"]);
        break;
      case "agent.activity":
        schedule(e.data?.settings ? ["agents", "settings"] : ["agents"]);
        break;
      case "env.deployed":
        schedule(["environments", "versions", "timeline", "map"]);
        break;
      case "project_version.changed":
        schedule(["versions", "timeline", "map"]);
        break;
    }
  });

  return { ...d, connected, refresh: (s: readonly Slice[] = SLICES) => load(s) };
}

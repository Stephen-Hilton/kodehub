// App data: loads every slice the UI shows and refreshes the right ones on server events.
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type Changeset, type Group, type Json, type MapData, type Requirement } from "./api";
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
  groups: Group[];
  summary: Record<string, number>;
  changesets: Changeset[];
  agents: Json;
  settings: Json;
  timeline: Json[];
  environments: Json[];
  releases: Json[];
  connected: boolean;
  error: string | null;
  loading: boolean;
}

const SLICES = ["map", "requirements", "changesets", "agents", "settings", "timeline", "environments", "releases"] as const;
type Slice = (typeof SLICES)[number];

export function useKodehub(at: string | null) {
  const [d, setD] = useState<Omit<KodehubData, "connected">>({
    map: null, requirements: [], groups: [], summary: {}, changesets: [], agents: { agents: [] }, settings: null,
    timeline: [], environments: [], releases: [], error: null, loading: true,
  });
  const atRef = useRef(at);
  atRef.current = at;

  const load = useCallback(async (which: readonly Slice[]) => {
    const tasks: Promise<Partial<KodehubData>>[] = which.map(async (s): Promise<Partial<KodehubData>> => {
      switch (s) {
        case "map": return { map: await api.map(atRef.current) };
        case "requirements": {
          const r = await api.requirements();
          return { requirements: r.requirements, groups: r.groups, summary: r.summary };
        }
        case "changesets": return { changesets: await api.changesets() };
        case "agents": return { agents: await api.agents() };
        case "settings": return { settings: await api.settings() };
        case "timeline": return { timeline: (await api.timeline()).events };
        case "environments": return { environments: await api.environments() };
        case "releases": return { releases: await api.releases() };
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
        schedule(["map", "requirements", "timeline", "changesets"]);
        break;
      case "changeset.updated":
        schedule(["changesets", "map", "requirements", "timeline"]);
        break;
      case "test.result":
        schedule(["map", "requirements"]);
        break;
      case "agent.activity":
        schedule(e.data?.settings ? ["agents", "settings"] : ["agents"]);
        break;
      case "env.deployed":
        schedule(["environments", "releases", "timeline"]);
        break;
    }
  });

  return { ...d, connected, refresh: (s: readonly Slice[] = SLICES) => load(s) };
}

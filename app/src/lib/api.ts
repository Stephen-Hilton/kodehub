// Typed client for the Kodehub HTTP API. The UI never talks to the database directly.

/* eslint-disable @typescript-eslint/no-explicit-any */
export type Json = any;

export type Health = "pass" | "fail" | "at_risk" | "uncovered" | "orphan" | "ghost";

export interface MapNode {
  id: string;
  key: string;
  level: "context" | "container" | "component" | "symbol";
  label: string;
  parent: string | null;
  kind?: string;
  language?: string;
  health: Health;
  tests?: number;
  tags?: string[];
  description?: string;
  ghost?: boolean;
  changeset?: string;
  action?: string;
  proposed_in?: string[];
  version?: string | null;
  version_at?: string | null;
  changed_since?: boolean;
  /** v0.1 in-node badges (spec §8.3). */
  lifecycle?: "planned" | "in_build" | "ready_for_test" | "live";
  requirements?: { own: Record<string, number>; total: Record<string, number> };
  test_counts?: { own: TestTally; total: TestTally };
  in_progress?: number;
  candidates?: number;
  seq?: number;
}

export interface TestTally {
  passed: number;
  failed: number;
  total: number;
  built: number;
  last_run?: string | null;
}

export interface MapEdge {
  id: string;
  source: string;
  target: string;
  kind: string;
}

export interface MapData {
  mission: Json;
  nodes: MapNode[];
  edges: MapEdge[];
  at: string | null;
}

export interface Requirement {
  _key: string;
  _id: string;
  title: string;
  statement: string;
  intent?: string;
  constraints: string[];
  examples: Example[];
  edge_cases: Example[];
  out_of_scope: string[];
  revision: number;
  at_risk: boolean;
  status: string;
  tests: { test: string; name: string; status: string; language: string }[];
  satisfiers: string[];
  path?: Json;
  history_count?: number;
}

export interface Example {
  description: string;
  input: Json;
  expected: Json;
}

export interface Group {
  id: string;
  key: string;
  title: string;
  logic: string;
  status: string;
  members: string[];
}

export interface Changeset {
  _key: string;
  title: string;
  intent: string;
  state: string;
  requirements: string[];
  plan: Json;
  proposals: Record<string, Json>;
  reviews: Json[];
  test_results: Json[];
  rounds: number;
  decisions: string[];
  activity: { role: string; message: string; at: string }[];
  history: { state: string; at: string; by: string; note: string }[];
  transcripts?: Json[];
  error?: string | null;
  created_at: string;
  merged_at?: string;
}

/** Base URL: same origin in the browser (Vite proxy / static serving); explicit in Tauri. */
export function apiBase(): string {
  const w = window as Json;
  const override = (() => {
    try {
      return localStorage.getItem("kodehub.server") ?? "";
    } catch {
      return "";
    }
  })();
  if (override) return override.replace(/\/$/, "");
  if (w.__TAURI_INTERNALS__) return "http://127.0.0.1:7878";
  return "";
}

function store(key: string): string | null {
  try { return localStorage.getItem(key); } catch { return null; }
}
function storeSet(key: string, v: string | null) {
  try { if (v === null) localStorage.removeItem(key); else localStorage.setItem(key, v); } catch { /* private mode */ }
}

/** Session token of the signed-in human (never sent anywhere but the Kodehub server). */
export const session = {
  token: (): string | null => store("kodehub.session"),
  set: (t: string | null) => storeSet("kodehub.session", t),
  project: (): string | null => store("kodehub.project"),
  setProject: (p: string | null) => storeSet("kodehub.project", p),
};

/** Query string carrying the session and project, for WebSockets (they cannot set headers). */
export function authQuery(): string {
  const q = new URLSearchParams();
  const t = session.token();
  const p = session.project();
  if (t) q.set("token", t);
  if (p) q.set("project", p);
  const s = q.toString();
  return s ? `?${s}` : "";
}

export function wsUrl(path: string): string {
  const base = apiBase();
  const full = path + (path.includes("?") ? authQuery().replace("?", "&") : authQuery());
  if (base) return base.replace(/^http/, "ws") + full;
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${location.host}${full}`;
}

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function call<T = Json>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers["content-type"] = "application/json";
  const token = session.token();
  if (token) headers["authorization"] = `Bearer ${token}`;
  const project = session.project();
  if (project) headers["x-kodehub-project"] = project;
  const res = await fetch(apiBase() + path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let data: Json = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    data = text;
  }
  if (res.status === 401 && !path.startsWith("/api/auth/")) {
    session.set(null);
    window.dispatchEvent(new Event("kodehub:signed-out"));
  }
  if (!res.ok) throw new ApiError(res.status, (data && data.error) || String(data) || res.statusText);
  return data as T;
}

export const api = {
  health: () => call("GET", "/health"),
  authStatus: () => call<{ has_users: boolean; open_registration: boolean }>("GET", "/api/auth/status"),
  login: (username: string, password: string) => call<{ token: string; user: Json }>("POST", "/api/auth/login", { username, password }),
  register: (username: string, password: string, display_name: string) =>
    call<{ token: string; user: Json }>("POST", "/api/auth/register", { username, password, display_name }),
  logout: () => call("POST", "/api/auth/logout"),
  me: () => call<Json>("GET", "/api/me"),
  projects: () => call<Json[]>("GET", "/api/projects"),
  createProject: (b: Json) => call<Json>("POST", "/api/projects", b),
  openProject: (id: string) => call<Json>("POST", `/api/projects/${id}/open`),
  commonContexts: () => call<{ name: string; description: string }[]>("GET", "/api/projects/contexts"),
  users: () => call<{ key: string; display_name: string }[]>("GET", "/api/users"),
  orgs: () => call<Json[]>("GET", "/api/orgs"),
  createOrg: (name: string) => call<Json>("POST", "/api/orgs", { name }),
  shares: (id: string) => call<Json>("GET", `/api/projects/${id}/shares`),
  share: (id: string, grantee: string, permission: string | null) => call<Json>("PUT", `/api/projects/${id}/shares`, { grantee, permission }),
  // v0.1: philosophy, cascade, use cases, lifecycle
  philosophy: () => call<Json>("GET", "/api/philosophy"),
  setPhilosophy: (text: string, raw_transcript?: string) => call<Json>("PUT", "/api/philosophy", { text, raw_transcript }),
  cascade: (node: string) => call<Json>("GET", `/api/cascade?node=${encodeURIComponent(node)}`),
  requirementsBy: (q: { kind?: string; level?: string }) =>
    call<Json[]>("GET", `/api/requirements-by?${new URLSearchParams(q as Record<string, string>).toString()}`),
  pushDown: () => call<Json[]>("GET", "/api/push-down"),
  nodeTests: (node: string) => call<Json[]>("GET", `/api/node-tests?node=${encodeURIComponent(node)}`),
  nodeVersions: (node: string) => call<Json[]>("GET", `/api/node-versions?node=${encodeURIComponent(node)}`),
  useCases: () => call<Json[]>("GET", "/api/use-cases"),
  createUseCase: (b: Json) => call<Json>("POST", "/api/use-cases", b),
  linkUseCase: (key: string, edge: string, to: string) => call<Json>("POST", `/api/use-cases/${key}/links`, { edge, to }),
  setUseCaseState: (key: string, state: string) => call<Json>("POST", `/api/use-cases/${key}/state`, { state }),
  // work queue, schedules, approvals, notifications
  workItems: (state?: string) => call<Json[]>("GET", `/api/work-items${state ? `?state=${state}` : ""}`),
  queueStats: () => call<Json>("GET", "/api/queue/stats"),
  cancelWork: (key: string) => call<Json>("POST", `/api/work-items/${key}/cancel`),
  unblock: () => call<Json>("POST", "/api/work-items/unblock"),
  schedules: () => call<Json[]>("GET", "/api/schedules"),
  replanSchedules: () => call<Json[]>("POST", "/api/schedules/replan"),
  setScheduleEnabled: (key: string, enabled: boolean) => call<Json>("POST", `/api/schedules/${key}/enabled`, { enabled }),
  testNow: (target: string) => call<Json>("POST", "/api/test-now", { target }),
  approvals: (state = "pending") => call<Json[]>("GET", `/api/approvals?state=${state}`),
  decide: (key: string, approve: boolean, note = "") => call<Json>("POST", `/api/approvals/${key}/decide`, { approve, note }),
  notifications: () => call<Json[]>("GET", "/api/notifications?unread=true"),
  readNotification: (key: string) => call<Json>("POST", `/api/notifications/${key}/read`),
  agentTypes: () => call<Json[]>("GET", "/api/agent-types"),
  // layered settings, credentials, agent users and tokens
  scopeSettings: (scope: string) => call<Json>("GET", `/api/scopes/${scope}/settings`),
  setSetting: (scope: string, key: string, value: Json) => call<Json>("PUT", `/api/scopes/${scope}/settings/${key}`, { value }),
  clearSetting: (scope: string, key: string) => call<Json>("DELETE", `/api/scopes/${scope}/settings/${key}`),
  credentials: (scope: string) => call<Json[]>("GET", `/api/credentials?scope=${encodeURIComponent(scope)}`),
  putCredential: (b: { scope: string; provider: string; kind: string; secret: string; label?: string }) => call<Json>("POST", "/api/credentials", b),
  deleteCredential: (id: string) => call<Json>("DELETE", `/api/credentials/${id}`),
  agentUsers: () => call<Json[]>("GET", "/api/agent-users"),
  createAgentUser: (name: string) => call<Json>("POST", "/api/agent-users", { name }),
  tokens: () => call<Json[]>("GET", "/api/tokens"),
  createToken: (agent: string, name: string, projects: string[] | null, permission: string) =>
    call<Json>("POST", "/api/tokens", { agent, name, projects, permission }),
  revokeToken: (id: string) => call<Json>("DELETE", `/api/tokens/${id}`),
  gitImport: (repo_url: string) => call<Json>("POST", "/api/git/import", { repo_url }),
  gitExport: (repo_url: string, branch: string) => call<Json>("POST", "/api/git/export", { repo_url, branch }),
  backup: () => call<Json>("POST", "/api/backups"),
  seed: (path: string) => call("POST", "/api/seed", { path }),
  map: (at?: string | null) => call<MapData>("GET", "/api/map" + (at ? `?at=${encodeURIComponent(at)}` : "")),
  requirements: () => call<{ requirements: Requirement[]; groups: Group[]; summary: Record<string, number> }>("GET", "/api/requirements"),
  requirement: (key: string) => call<Requirement>("GET", `/api/requirements/${key}`),
  requirementHistory: (key: string) => call<Json[]>("GET", `/api/requirements/${key}/history`),
  updateRequirement: (key: string, patch: Json) => call("PATCH", `/api/requirements/${key}`, { patch }),
  addEdgeCase: (key: string, e: Example) => call("POST", `/api/requirements/${key}/edge_cases`, e),
  addExample: (key: string, e: Example & { run?: string }) => call("POST", `/api/requirements/${key}/examples`, e),
  createRequirement: (r: Json) => call("POST", "/api/requirements", r),
  symbol: (key: string) => call<Json>("GET", `/api/symbols/${key}`),
  symbols: () => call<Json[]>("GET", "/api/symbols"),
  run: (symbol: string, args: Json, environment: string, token?: string) =>
    call<Json>("POST", "/api/run", { symbol, args, environment, token }),
  runTests: (scope?: Json) => call<Json>("POST", "/api/tests/run", { scope: scope ?? { kind: "all" } }),
  changesets: () => call<Changeset[]>("GET", "/api/changesets"),
  changeset: (key: string) => call<Changeset>("GET", `/api/changesets/${key}`),
  openChangeset: (b: Json) => call<Changeset>("POST", "/api/changesets", b),
  proposeVersion: (key: string, b: Json) => call("POST", `/api/changesets/${key}/versions`, b),
  manualEdit: (symbol: string, body: string) => call("POST", "/api/manual-edit", { symbol, body }),
  resolve: (key: string, choice: string, rationale: string, to: "building" | "testing") =>
    call("POST", `/api/changesets/${key}/resolve`, { choice, rationale, to }),
  applyChat: (key: string, apply: boolean) => call("POST", `/api/chat/${key}/apply`, { apply }),
  approveProd: (release: string, env = "prod") => call<{ approval: string }>("POST", `/api/environments/${env}/approvals`, { release }),
  plan: (key: string, plan: Json) => call("POST", `/api/changesets/${key}/plan`, { plan, by: "human" }),
  approve: (key: string) => call("POST", `/api/changesets/${key}/approve`, { by: "human" }),
  reject: (key: string, reason: string) => call("POST", `/api/changesets/${key}/reject`, { by: "human", reason }),
  merge: (key: string) => call("POST", `/api/changesets/${key}/merge`, { by: "human" }),
  resume: (key: string) => call("POST", `/api/changesets/${key}/run`),
  replan: (key: string) => call("POST", `/api/changesets/${key}/replan`),
  transition: (key: string, to: string, note = "") => call("POST", `/api/changesets/${key}/transition`, { to, note, by: "human" }),
  agents: () => call<Json>("GET", "/api/agents"),
  chatHistory: () => call<Json[]>("GET", "/api/chat"),
  chat: (message: string, references: string[]) => call<Json>("POST", "/api/chat", { message, references }),
  decisions: () => call<Json[]>("GET", "/api/decisions"),
  timeline: () => call<{ events: Json[] }>("GET", "/api/timeline"),
  coverage: () => call<Json>("GET", "/api/coverage"),
  orphans: () => call<Json[]>("GET", "/api/orphans"),
  duplicates: () => call<Json[]>("GET", "/api/duplicates"),
  nearDuplicates: (t: number) => call<{ exact: Json[]; near: Json[] }>("GET", `/api/duplicates?near=${t}`),
  settings: () => call<Json>("GET", "/api/settings"),
  patchAgents: (p: Json) => call<Json>("PATCH", "/api/settings/agents", p),
  releases: () => call<Json[]>("GET", "/api/releases"),
  cutRelease: (name: string, notes: string, rule?: Json) => call<Json>("POST", "/api/releases", { name, notes, rule }),
  diff: (a: string, b: string) => call<Json>("GET", `/api/releases/${a}/diff/${b}`),
  environments: () => call<Json[]>("GET", "/api/environments"),
  promote: (env: string, release: string, approval?: string) =>
    call<Json>("POST", `/api/environments/${env}/promote`, { release, approval }),
  rollback: (env: string, approval?: string) => call<Json>("POST", `/api/environments/${env}/rollback`, { approval }),
  cloneRelease: (key: string, environment: string) => call<Json>("POST", `/api/releases/${key}/clone`, { environment }),
  patchRelease: (key: string, swaps: { symbol: string; version: string }[], approval?: string) =>
    call<Json>("POST", `/api/releases/${key}/patch`, { swaps, approval }),
  mcpCommand: () => call<{ command: string }>("GET", "/api/terminal/mcp-command"),
};

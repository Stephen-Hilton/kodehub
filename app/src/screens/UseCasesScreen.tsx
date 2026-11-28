// Use cases (spec §15): what a user of the product would do and expect, with the requirements
// it relates to and the tests that define its expected outcome. Draft → live (after an
// architect agent checks it against the live ones).
import { useEffect, useState } from "react";
import { api, localKey, type Json } from "../lib/api";
import { useToast } from "../components/Toasts";

export function UseCasesScreen({ requirements, tests }: { requirements: Json[]; tests: { id: string; label: string }[] }) {
  const toast = useToast();
  const [ucs, setUcs] = useState<Json[] | null>(null);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [reqs, setReqs] = useState<string[]>([]);
  const load = () => api.useCases().then(setUcs).catch((e) => toast("error", e.message));
  useEffect(() => { load(); }, []); // eslint-disable-line react-hooks/exhaustive-deps
  const create = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      await api.createUseCase({ name, description, requirements: reqs });
      setName(""); setDescription(""); setReqs([]);
      load();
    } catch (err) { toast("error", (err as Error).message); }
  };
  return (
    <div className="screen" data-testid="use-cases-screen">
      <div className="screen-head"><h2>Use cases</h2><span className="faint">Agents work to satisfy these, not requirements in a vacuum.</span></div>
      <form className="card form" onSubmit={create}>
        <div className="form-grid">
          <label className="form-field"><span className="label">A user…</span>
            <input className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder="signs in and sees their projects" required data-testid="uc-name" /></label>
          <label className="form-field"><span className="label">…and expects</span>
            <input className="input" value={description} onChange={(e) => setDescription(e.target.value)} placeholder="the project list right after signing in" /></label>
        </div>
        <div className="chips">{requirements.map((r) => (
          <label key={r._key} className="chip"><input type="checkbox" checked={reqs.includes(r._key)}
            onChange={() => setReqs((x) => (x.includes(r._key) ? x.filter((y) => y !== r._key) : [...x, r._key]))} /> {r.title}</label>
        ))}</div>
        <div className="row"><span className="spacer" /><button className="btn primary" data-testid="uc-create">Add use case (draft)</button></div>
      </form>
      <div className="uc-list">
        {ucs?.length === 0 && <div className="empty">No use cases yet.</div>}
        {ucs?.map((u) => <UseCaseCard key={u._key} uc={u} tests={tests} onChange={load} />)}
      </div>
    </div>
  );
}

function UseCaseCard({ uc, tests, onChange }: { uc: Json; tests: { id: string; label: string }[]; onChange: () => void }) {
  const toast = useToast();
  const [pick, setPick] = useState("");
  const tone = uc.state === "live" ? "pass" : uc.state === "checking" ? "risk" : "";
  return (
    <div className="card uc" data-testid={`uc-${localKey(uc._key)}`}>
      <div className="row">
        <b>{uc.name}</b>
        <span className={`badge ${tone}`} data-testid={`uc-state-${localKey(uc._key)}`}>{uc.state}</span>
        <span className="spacer" />
        {uc.state === "draft" && (
          <button className="btn sm primary" data-testid={`uc-live-${localKey(uc._key)}`} disabled={!uc.tests?.length} title={uc.tests?.length ? "" : "Add an expected-outcome test first"}
            onClick={() => api.setUseCaseState(uc._key, "live").then(onChange).catch((e) => toast("error", e.message))}>Go live</button>
        )}
        {uc.state !== "draft" && (
          <button className="btn sm" onClick={() => api.setUseCaseState(uc._key, "draft").then(onChange).catch((e) => toast("error", e.message))}>Back to draft</button>
        )}
      </div>
      {uc.description && <div className="muted">{uc.description}</div>}
      {uc.check && !uc.check.ok && <div className="signin-error">Architect check: {uc.check.notes}</div>}
      <div className="faint" style={{ fontSize: 12 }}>
        Requirements: {uc.requirements?.map((r: Json) => r.title).join(", ") || "—"} · Expected-outcome tests: {uc.tests?.map((t: Json) => t.name).join(", ") || "none yet"}
      </div>
      <div className="row">
        <select className="input" value={pick} onChange={(e) => setPick(e.target.value)} data-testid={`uc-test-pick-${localKey(uc._key)}`}>
          <option value="">Add an expected-outcome test…</option>
          {tests.map((t) => <option key={t.id} value={t.id}>{t.label}</option>)}
        </select>
        <button className="btn sm" disabled={!pick} data-testid={`uc-test-add-${localKey(uc._key)}`}
          onClick={() => api.linkUseCase(uc._key, "verified_by", pick).then(() => { setPick(""); onChange(); }).catch((e) => toast("error", e.message))}>Link</button>
      </div>
    </div>
  );
}

// Sign-in / first-account screen (spec §6, §13A). The session token is kept in localStorage and
// sent only to the Kodehub server.
import { useEffect, useState } from "react";
import { api, session } from "../lib/api";
import { Logo } from "./icons";

export function SignIn({ onSignedIn }: { onSignedIn: () => void }) {
  const [status, setStatus] = useState<{ has_users: boolean; open_registration: boolean } | null>(null);
  const [mode, setMode] = useState<"login" | "register">("login");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api.authStatus().then((s) => { setStatus(s); if (!s.has_users) setMode("register"); }).catch((e) => setError(e.message));
  }, []);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const r = mode === "login" ? await api.login(username, password) : await api.register(username, password, displayName);
      session.set(r.token);
      onSignedIn();
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const first = status && !status.has_users;
  return (
    <div className="signin" data-testid="signin">
      <form className="signin-card glass" onSubmit={submit}>
        <div className="signin-brand"><Logo /> <span>Kodehub</span></div>
        <h1 className="signin-title">{first ? "Create the first account" : mode === "login" ? "Sign in" : "Create an account"}</h1>
        {first && <p className="muted signin-note">The first account administers this Kodehub installation.</p>}
        <label className="form-field">
          <span className="label">Username</span>
          <input className="input" data-testid="signin-username" autoFocus autoComplete="username" value={username} onChange={(e) => setUsername(e.target.value)} />
        </label>
        {mode === "register" && (
          <label className="form-field">
            <span className="label">Display name</span>
            <input className="input" data-testid="signin-display" value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
          </label>
        )}
        <label className="form-field">
          <span className="label">Password</span>
          <input className="input" data-testid="signin-password" type="password" autoComplete={mode === "login" ? "current-password" : "new-password"} value={password} onChange={(e) => setPassword(e.target.value)} />
        </label>
        {error && <div className="signin-error" role="alert">{error}</div>}
        <button className="btn primary" data-testid="signin-submit" disabled={busy || !username || !password}>
          {busy ? "…" : mode === "login" ? "Sign in" : "Create account"}
        </button>
        {status?.has_users && status.open_registration && (
          <button type="button" className="btn ghost-btn" onClick={() => setMode(mode === "login" ? "register" : "login")}>
            {mode === "login" ? "Create an account" : "I have an account"}
          </button>
        )}
      </form>
    </div>
  );
}

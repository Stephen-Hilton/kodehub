import { createContext, useCallback, useContext, useState, type ReactNode } from "react";
import { Alert, Check, X } from "./icons";

type Kind = "info" | "success" | "error";
interface Toast { id: number; kind: Kind; text: string; action?: { label: string; run: () => void } }
const Ctx = createContext<(kind: Kind, text: string, action?: Toast["action"]) => void>(() => {});
export const useToast = () => useContext(Ctx);

export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<Toast[]>([]);
  const push = useCallback((kind: Kind, text: string, action?: Toast["action"]) => {
    const id = Date.now() + Math.random();
    // one toast per message; at most three on screen
    setItems((t) => [...t.filter((x) => x.text !== text).slice(-2), { id, kind, text, action }]);
    window.setTimeout(() => setItems((t) => t.filter((x) => x.id !== id)), action ? 12000 : kind === "error" ? 7000 : 4500);
  }, []);
  return (
    <Ctx.Provider value={push}>
      {children}
      <div className="toasts" role="status">
        {items.map((t) => (
          <div key={t.id} className={`toast glass ${t.kind}`}>
            <span style={{ color: t.kind === "error" ? "var(--fail)" : t.kind === "success" ? "var(--pass)" : "var(--accent)", marginTop: 1 }}>
              {t.kind === "error" ? <Alert size={15} /> : <Check size={15} />}
            </span>
            <div style={{ flex: 1 }}>
              {t.text}
              {t.action && (
                <div style={{ marginTop: 8 }}>
                  <button className="btn sm primary" onClick={() => { t.action!.run(); setItems((x) => x.filter((y) => y.id !== t.id)); }}>
                    {t.action.label}
                  </button>
                </div>
              )}
            </div>
            <button className="btn ghost-btn icon sm" aria-label="Dismiss" onClick={() => setItems((x) => x.filter((y) => y.id !== t.id))}>
              <X size={13} />
            </button>
          </div>
        ))}
      </div>
    </Ctx.Provider>
  );
}

// Speech capture for the philosophy document and requirements (spec §14.1). The browser only
// records; the audio goes to the Kodehub server, which transcribes it with a local Whisper model
// (no browser speech service, so Chrome and Tauri's WebKit behave the same).
import { useRef, useState } from "react";
import { apiBase, session } from "../lib/api";
import { useToast } from "./Toasts";

export function Recorder({ onText }: { onText: (t: string) => void }) {
  const toast = useToast();
  const [state, setState] = useState<"idle" | "recording" | "transcribing">("idle");
  const rec = useRef<MediaRecorder | null>(null);
  const chunks = useRef<Blob[]>([]);
  if (typeof window === "undefined" || !("MediaRecorder" in window) || !navigator.mediaDevices?.getUserMedia) return null;
  const start = async () => {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const r = new MediaRecorder(stream);
      chunks.current = [];
      r.ondataavailable = (e) => { if (e.data.size) chunks.current.push(e.data); };
      r.onstop = async () => {
        stream.getTracks().forEach((t) => t.stop());
        setState("transcribing");
        try {
          const blob = new Blob(chunks.current, { type: r.mimeType || "audio/webm" });
          const res = await fetch(`${apiBase()}/api/transcribe`, {
            method: "POST",
            headers: { authorization: `Bearer ${session.token() ?? ""}`, "content-type": blob.type },
            body: blob,
          });
          const j = await res.json();
          if (!res.ok) throw new Error(j.error ?? res.statusText);
          onText(j.text);
        } catch (e) { toast("error", `Transcription failed: ${(e as Error).message}`); }
        setState("idle");
      };
      r.start();
      rec.current = r;
      setState("recording");
    } catch (e) { toast("error", `Microphone unavailable: ${(e as Error).message}`); }
  };
  return (
    <button type="button" className={`btn sm ${state === "recording" ? "danger" : ""}`} data-testid="record"
      onClick={() => (state === "recording" ? rec.current?.stop() : state === "idle" ? start() : undefined)} disabled={state === "transcribing"}>
      {state === "recording" ? "■ Stop and transcribe" : state === "transcribing" ? "Transcribing…" : "● Speak it"}
    </button>
  );
}

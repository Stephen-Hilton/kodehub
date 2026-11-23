// Live server events over WebSocket with automatic reconnect.
import { useEffect, useRef, useState } from "react";
import { wsUrl, type Json } from "./api";

export interface KodehubEvent {
  type: string;
  data: Json;
  at?: string;
}

export function useEvents(onEvent: (e: KodehubEvent) => void): boolean {
  const [connected, setConnected] = useState(false);
  const handler = useRef(onEvent);
  handler.current = onEvent;
  useEffect(() => {
    let ws: WebSocket | null = null;
    let closed = false;
    let retry = 500;
    let timer: number | undefined;
    const open = () => {
      ws = new WebSocket(wsUrl("/api/events"));
      ws.onopen = () => {
        setConnected(true);
        retry = 500;
      };
      ws.onmessage = (m) => {
        try {
          handler.current(JSON.parse(m.data));
        } catch {
          /* ignore malformed */
        }
      };
      ws.onclose = () => {
        setConnected(false);
        if (!closed) timer = window.setTimeout(open, (retry = Math.min(retry * 2, 8000)));
      };
    };
    open();
    return () => {
      closed = true;
      window.clearTimeout(timer);
      ws?.close();
    };
  }, []);
  return connected;
}

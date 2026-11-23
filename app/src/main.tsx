import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { initUiToken } from "./lib/api";

// Tauri overlay title bar: leave room for the window controls and make the banner draggable.
if ((window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__) document.body.classList.add("tauri");

await initUiToken();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

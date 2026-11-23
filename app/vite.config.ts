import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import fs from "node:fs";
import path from "node:path";

// Dev only: inject the server's per-launch UI secret (written to $KODEHUB_HOME/ui-token).
const home = process.env.KODEHUB_HOME ?? path.resolve(__dirname, "../.kode");
const uiToken = (): Plugin => ({
  name: "kodehub-ui-token",
  transformIndexHtml(html) {
    let token = "";
    try { token = fs.readFileSync(path.join(home, "ui-token"), "utf8").trim(); } catch { /* server not started */ }
    return token ? html.replace("</head>", `<meta name="kodehub-ui-token" content="${token}" /></head>`) : html;
  },
});

// The UI talks to kodehub-server over HTTP + WebSocket. In dev, Vite proxies /api and /health.
const server = process.env.KODEHUB_SERVER_URL ?? "http://127.0.0.1:7878";

export default defineConfig({
  plugins: [react(), uiToken()],
  clearScreen: false,
  build: { target: "es2022", chunkSizeWarningLimit: 1500 },
  server: {
    port: Number(process.env.KODEHUB_UI_PORT ?? 1420),
    strictPort: true,
    proxy: {
      "/api": { target: server, ws: true, changeOrigin: true },
      "/health": { target: server, changeOrigin: true },
    },
  },
});

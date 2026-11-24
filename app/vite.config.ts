import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The UI talks to kodehub-server over HTTP + WebSocket. In dev, Vite proxies /api and /health.
const server = process.env.KODEHUB_SERVER_URL ?? "http://127.0.0.1:7878";

export default defineConfig({
  plugins: [react()],
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

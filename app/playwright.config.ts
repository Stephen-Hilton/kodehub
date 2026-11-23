import { defineConfig } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// The e2e run uses its own database and KODEHUB_HOME with the recorded agent backend, so it is
// deterministic and needs no Claude credentials. Compiled dependencies and the Python venv are
// shared with the Rust integration tests.
const repo = path.resolve(__dirname, "..");
const home = path.join(repo, ".kode", "e2e");
fs.mkdirSync(home, { recursive: true });
fs.writeFileSync(path.join(home, "config.toml"), '[agents]\nbackend = "recorded"\n');
const db = process.env.KODEHUB_E2E_DB ?? `kodehub_e2e_${Date.now()}`;
process.env.KODEHUB_E2E_DB = db;
const SERVER_PORT = 7989;
const UI_PORT = 1431;

export default defineConfig({
  testDir: "./e2e",
  timeout: 300_000,
  expect: { timeout: 20_000 },
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  globalTeardown: "./e2e/teardown.ts",
  use: {
    baseURL: `http://localhost:${UI_PORT}`,
    viewport: { width: 1600, height: 960 },
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
  webServer: [
    {
      command: `${path.join(repo, "target/debug/kodehub-server")}`,
      url: `http://127.0.0.1:${SERVER_PORT}/health`,
      reuseExistingServer: false,
      timeout: 60_000,
      env: {
        KODEHUB_DB: db,
        KODEHUB_BIND: `127.0.0.1:${SERVER_PORT}`,
        KODEHUB_HOME: home,
        KODEHUB_CARGO_TARGET: path.join(repo, ".kode/test/cargo-target"),
        KODEHUB_PYENV: path.join(repo, ".kode/test/pyenv"),
        PATH: process.env.PATH ?? "",
        HOME: process.env.HOME ?? "",
      },
      stdout: "ignore",
      stderr: "pipe",
    },
    {
      command: `npx vite --port ${UI_PORT} --strictPort`,
      url: `http://localhost:${UI_PORT}`,
      reuseExistingServer: false,
      timeout: 60_000,
      env: { KODEHUB_SERVER_URL: `http://127.0.0.1:${SERVER_PORT}`, KODEHUB_UI_PORT: String(UI_PORT), KODEHUB_HOME: home },
    },
  ],
});

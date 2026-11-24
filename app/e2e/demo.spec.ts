// M6 acceptance: a person performs the doc 08 demo entirely in the app (no CLI).
import { expect, test, type Page } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { signIn } from "./auth";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const shots = path.resolve(__dirname, "../../.kode/shots/e2e");
const shot = async (page: Page, name: string) => {
  await page.waitForTimeout(700); // let map animations settle
  await page.screenshot({ path: path.join(shots, `${name}.png`) });
};
const repo = path.resolve(__dirname, "../..");

test.describe.configure({ mode: "serial" });

test("doc 08 demo, entirely in the desktop UI", async ({ page, request }) => {
  // 0. seed the project (setup, as a fresh install would ship it)
  const headers = await signIn(page, request);
  const seeded = await request.post("/api/seed", { headers, data: { path: path.join(repo, "examples/auth-poc") } });
  expect(seeded.ok()).toBeTruthy();

  await page.goto("/");
  await expect(page.getByTestId("mission-banner")).toContainText("never leaks information");
  await expect(page.locator(".req").first()).toBeVisible();

  // 1. start with everything green — run all tests from the UI
  await page.getByTestId("run-all-tests").click();
  await expect(page.getByTestId("kpi-passing")).toHaveText("2/2", { timeout: 120_000 });
  await expect(page.getByTestId("req-r1-generic-failure")).toContainText("Passing");
  const mapNodes = await page.evaluate(() => (window as any).__kodehubCy.nodes().length);
  expect(mapNodes).toBeGreaterThan(10);
  await shot(page, "01-green");

  // selecting a requirement highlights its path on the map
  await page.getByTestId("req-r1-generic-failure").click();
  await expect.poll(() => page.evaluate(() => (window as any).__kodehubCy.nodes(".hl").length)).toBeGreaterThan(3);
  await page.getByTestId("req-r1-generic-failure").click();

  // 2. human adds an edge case to R1 (one-click action)
  await page.getByTestId("add-edge-r1-generic-failure").click();
  const editor = page.getByTestId("requirement-editor");
  await expect(editor).toBeVisible();
  await page.getByTestId("edge-cases-desc-1").fill("A locked account must also return invalid_credentials");
  await page.getByTestId("edge-cases-input-1").fill('{"username": "alice", "password": "correct horse", "locked": true}');
  await page.getByTestId("edge-cases-expected-1").fill("invalid_credentials");
  await shot(page, "02-edge-case");
  await page.getByTestId("req-save").click();
  await expect(editor).toBeHidden();

  // 3. R1 turns amber; the orchestrator opens a changeset with ghost nodes
  await expect(page.getByTestId("req-r1-generic-failure")).toContainText("At risk");
  const planned = page.locator('[data-testid^="changeset-"][data-state="planned"]');
  await expect(planned).toBeVisible({ timeout: 60_000 });
  await expect.poll(() => page.evaluate(() => (window as any).__kodehubCy.nodes(".ghost").length)).toBeGreaterThan(0);
  await expect.poll(() => page.evaluate(() => (window as any).__kodehubCy.nodes(".h-at_risk").length)).toBeGreaterThan(0);
  await shot(page, "03-planned-ghosts");

  // 4. human approves the plan → 5–7 tester, coder, blind reviewer, decision, tests, merge
  await planned.getByTestId("cs-approve").click();
  const merged = page.locator('[data-testid^="changeset-"][data-state="merged"]');
  await expect(merged).toBeVisible({ timeout: 240_000 });
  await expect(merged).toContainText("decision");
  await merged.getByRole("button", { name: "Details" }).click();
  await expect(merged.getByTestId("findings")).toContainText("Timing difference");
  await expect(merged).toContainText("decision");
  await expect(page.getByTestId("req-r1-generic-failure")).toContainText("Passing", { timeout: 60_000 });
  await expect(page.getByTestId("kpi-passing")).toHaveText("2/2");
  await shot(page, "04-merged-green");
  await page.getByTestId("tab-decisions").click();
  await expect(page.getByTestId("decisions")).toContainText("Review: Timing");
  await page.getByTestId("tab-changesets").click();

  // 8. run now with a locked user. First hash a real password in the app (run hash_password),
  // prove the same user authenticates when not locked, then that locking it is rejected.
  await page.getByTestId("map-search").fill("hash_pass");
  await page.getByTestId("search-result-hash_password").click();
  let panel = page.getByTestId("node-panel");
  await expect(panel.getByTestId("np-name")).toHaveText("hash_password");
  await panel.getByTestId("arg-password").fill("correct horse");
  await panel.getByTestId("run-now").click();
  await expect(panel.getByTestId("run-result")).toContainText("$argon2id$", { timeout: 120_000 });
  const hash = JSON.parse((await panel.getByTestId("run-result").textContent())!.replace(/^Copy/, ""));

  await page.getByTestId("map-search").fill("verify_cred");
  await page.getByTestId("search-result-verify_credentials").click();
  panel = page.getByTestId("node-panel");
  await expect(panel.getByTestId("np-name")).toHaveText("verify_credentials");
  await panel.getByTestId("arg-store-users-add").click();
  await panel.getByTestId("arg-store-users-key-0").fill("alice");
  await panel.getByTestId("arg-store-users-0-username").fill("alice");
  // the hash from the previous run is one click away
  await panel.getByTestId("arg-store-users-0-password_hash-paste").click();
  await expect(panel.getByTestId("arg-store-users-0-password_hash")).toHaveValue(hash);
  await panel.getByTestId("arg-username").fill("alice");
  await panel.getByTestId("arg-password").fill("correct horse");
  await panel.getByTestId("run-now").click();
  await expect(panel.getByTestId("run-result")).toContainText("authenticated", { timeout: 120_000 });
  await panel.getByTestId("arg-store-users-0-locked").check();
  let meta = "";
  for (let i = 0; i < 6 && !meta.includes("cache hit"); i++) {
    await panel.getByTestId("run-now").click();
    await expect(panel.getByTestId("run-result")).toContainText("invalid_credentials", { timeout: 120_000 });
    meta = (await panel.getByTestId("run-meta").textContent()) ?? "";
  }
  expect(meta).toContain("cache hit");
  await shot(page, "05-run-now");

  // save the run as an example
  await panel.getByTestId("save-example").click();
  await panel.getByTestId("example-desc").fill("Locked user with the right password");
  await panel.getByTestId("example-save").click();
  await expect(page.locator(".toast", { hasText: "Saved as example" })).toBeVisible();
  // saving an example is a requirement edit: the agents turn it into a test and merge — green again
  await page.getByRole("button", { name: "Close panel" }).click();
  const example = page.locator('[data-testid^="changeset-"][data-state="planned"]');
  await expect(example).toBeVisible({ timeout: 60_000 });
  await example.getByTestId("cs-approve").click();
  await expect(page.getByTestId("req-r1-generic-failure")).toContainText("Passing", { timeout: 240_000 });
  await expect(page.getByTestId("kpi-passing")).toHaveText("2/2");

  // 9. cut a release and promote it to test
  await page.getByTestId("open-releases").click();
  await page.getByTestId("release-name").fill("demo-1");
  await page.getByTestId("cut-release").click();
  await expect(page.getByTestId("env-dev-release")).toHaveText("demo-1");
  await page.getByTestId("promote-test").click();
  await expect(page.getByTestId("env-test-release")).toHaveText("demo-1", { timeout: 120_000 });
  await expect(page.getByTestId("gate-report")).toContainText("passed");
  await shot(page, "06-release-promoted");
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Close" }).click();

  // time slider: view the system as it was at the seed — every seed symbol, no ghosts
  await page.getByTestId("time-stop-0").click();
  await expect(page.getByTestId("history-banner")).toBeVisible();
  await expect.poll(() => page.evaluate(() => (window as any).__kodehubCy.nodes(".symbol").length)).toBeGreaterThanOrEqual(10);
  expect(await page.evaluate(() => (window as any).__kodehubCy.nodes(".ghost").length)).toBe(0);
  await shot(page, "07-history");
  await page.getByTestId("time-live").click();
  await expect(page.getByTestId("history-banner")).toBeHidden();

  // embedded terminal
  await page.getByTestId("toggle-terminal").click();
  await expect(page.getByTestId("terminal")).toBeVisible();
  await page.locator(".xterm").click();
  await page.waitForTimeout(800);
  await page.keyboard.type("echo kodehub-$((40+2))\n");
  await expect(page.locator(".xterm-rows")).toContainText("kodehub-42", { timeout: 15_000 });
  await shot(page, "08-terminal");
});

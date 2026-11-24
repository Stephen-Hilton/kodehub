// v0.1 UI: projects (create with a template), in-node badges and overlays, the screens that
// replaced the sidebars, use cases, work, settings and credentials.
import { expect, test } from "@playwright/test";
import { signIn } from "./auth";

test.describe.configure({ mode: "serial" });

test("create a project, see badges, settings, use cases and work", async ({ page, request }) => {
  const headers = await signIn(page, request);
  await page.goto("/");
  await expect(page.getByTestId("topnav")).toBeVisible();

  // projects: create one from the master template, with a philosophy and an extra context
  await page.getByTestId("project-switch").selectOption("__all");
  await expect(page.getByTestId("projects-screen")).toBeVisible();
  await page.getByTestId("new-project").click();
  await page.getByTestId("project-name").fill("Shop");
  await page.getByTestId("project-philosophy").fill("Checkout is fast and never loses an order.");
  await page.getByTestId("project-template").selectOption("master");
  await page.getByTestId("create-project-submit").click();
  // the new project opens on its map: planned contexts with lifecycle and requirement badges
  await expect(page.getByTestId("mission-banner")).toContainText("never loses an order", { timeout: 30_000 });
  await page.getByTestId("tab-map").click();
  await expect(page.getByTestId("badge-life-contexts/data-platform")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("badge-reqs-contexts/data-platform")).toContainText("B");
  await page.getByTestId("badge-life-contexts/data-platform").click();
  await expect(page.getByTestId("overlay-lifecycle")).toContainText("Planned");
  await page.keyboard.press("Escape");
  await page.getByTestId("badge-reqs-contexts/data-platform").click();
  await expect(page.getByTestId("cascade")).toContainText("Secrets are encrypted at rest");
  await page.keyboard.press("Escape");

  // there are no side panels for ordinary detail: requirements live on their own screen
  await expect(page.getByTestId("requirements-panel")).toHaveCount(0);
  await page.getByTestId("tab-requirements").click();
  await expect(page.getByTestId("requirements-screen")).toBeVisible();
  await expect(page.getByTestId("philosophy-text")).toHaveValue(/never loses an order/);

  // work: the kickoff chain was queued (architect first, then a planner per context)
  await page.getByTestId("tab-work").click();
  await expect(page.getByTestId("work-screen")).toBeVisible();
  await page.getByRole("button", { name: "all" }).click();
  await expect(page.locator('[data-testid^="work-"]').first()).toBeVisible({ timeout: 30_000 });

  // settings: layered values and the subscription-token warning
  await page.getByTestId("tab-settings").click();
  await expect(page.getByTestId("setting-human_engagement")).toContainText("from");
  await page.getByTestId("credentials").locator("select").first().selectOption("subscription_token");
  await expect(page.getByTestId("subscription-warning")).toContainText("not for production or for organizations");

  // use cases (in the seeded demo project): draft, link an expected-outcome test, go live
  const projects = await (await request.get("/api/projects", { headers })).json();
  const demo = projects.find((p: { name: string }) => p.name !== "Shop");
  await request.put(`/api/scopes/project:${demo.id}/settings/use_case_check`, { headers: { ...headers, "x-kodehub-project": demo.id }, data: { value: false } });
  await page.getByTestId("project-switch").selectOption(demo.id);
  await page.getByTestId("tab-use_cases").click();
  await page.getByTestId("uc-name").fill("signs in with the right password");
  await page.getByTestId("uc-create").click();
  const uc = page.locator('[data-testid^="uc-signs-in"]').first();
  await expect(uc).toContainText("draft");
  await uc.locator("select").selectOption({ label: "test_valid_login_succeeds" });
  await uc.getByRole("button", { name: "Link" }).click();
  await uc.getByRole("button", { name: "Go live" }).click();
  await expect(uc).toContainText("live");
});

// Resizable panes: drag handles resize the sidebars, node panel and terminal; sizes persist.
import { expect, test, type Locator, type Page } from "@playwright/test";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(__dirname, "../..");
const shots = path.resolve(__dirname, "../../.kode/shots/e2e");

const width = async (l: Locator) => (await l.boundingBox())!.width;
const height = async (l: Locator) => (await l.boundingBox())!.height;

async function drag(page: Page, handle: Locator, dx: number, dy: number) {
  const b = (await handle.boundingBox())!;
  const x = b.x + b.width / 2, y = b.y + b.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + dx / 2, y + dy / 2, { steps: 4 });
  await page.mouse.move(x + dx, y + dy, { steps: 4 });
  await page.mouse.up();
}

test("panes resize by dragging, clamp, persist and reset", async ({ page, request }) => {
  const seeded = await request.post("/api/seed", { data: { path: path.join(repo, "examples/auth-poc") } });
  expect(seeded.ok()).toBeTruthy();
  await page.goto("/");
  await page.evaluate(() => localStorage.removeItem("kodehub.layout.v1"));
  await page.reload();

  const left = page.getByTestId("requirements-panel");
  const right = page.getByTestId("agents-panel");
  await expect(left).toBeVisible();
  const left0 = await width(left);
  const right0 = await width(right);

  // left sidebar grows by the drag distance
  await drag(page, page.getByTestId("splitter-left"), 120, 0);
  await expect.poll(() => width(left)).toBeCloseTo(left0 + 120, -1);
  // right sidebar grows when dragged left
  await drag(page, page.getByTestId("splitter-right"), -80, 0);
  await expect.poll(() => width(right)).toBeCloseTo(right0 + 80, -1);
  // clamped at the maximum
  await drag(page, page.getByTestId("splitter-left"), 2000, 0);
  await expect.poll(() => width(left)).toBeLessThanOrEqual(600);

  // keyboard: arrows step 16px
  await drag(page, page.getByTestId("splitter-left"), -2000, 0);
  await expect.poll(() => width(left)).toBeCloseTo(240, -1);
  await page.getByTestId("splitter-left").focus();
  await page.keyboard.press("ArrowRight");
  await expect.poll(() => width(left)).toBeCloseTo(256, -1);
  await expect(page.getByTestId("splitter-left")).toHaveAttribute("aria-valuenow", "256");

  // node panel handle appears with the panel
  await page.getByTestId("map-search").fill("verify_cred");
  await page.getByTestId("search-result-verify_credentials").click();
  const panel = page.getByTestId("node-panel");
  await expect(panel).toBeVisible();
  const panel0 = await width(panel);
  await drag(page, page.getByTestId("splitter-panel"), -100, 0);
  await expect.poll(() => width(panel)).toBeCloseTo(panel0 + 100, -1);
  // the map never gets narrower than 320px
  await drag(page, page.getByTestId("splitter-panel"), -2000, 0);
  const mapW = await page.evaluate(() => parseFloat(getComputedStyle(document.querySelector(".main")!).gridTemplateColumns.split(" ")[1]));
  expect(mapW).toBeGreaterThanOrEqual(319);
  await drag(page, page.getByTestId("splitter-panel"), 2000, 0);

  // terminal height
  await page.keyboard.press("Escape");
  await page.keyboard.press("Control+`");
  const term = page.getByTestId("terminal");
  await expect(term).toBeVisible();
  const term0 = await height(term);
  await drag(page, page.getByTestId("splitter-term"), 0, -150);
  await expect.poll(() => height(term)).toBeCloseTo(term0 + 150, -1);
  await page.waitForTimeout(400);
  await page.screenshot({ path: path.join(shots, "panes-resized.png") });

  // persisted across reload
  const leftNow = await width(left);
  const rightNow = await width(right);
  await page.reload();
  await expect(left).toBeVisible();
  expect(await width(left)).toBeCloseTo(leftNow, 0);
  expect(await width(right)).toBeCloseTo(rightNow, 0);
  await page.keyboard.press("Control+`");
  await expect.poll(() => height(page.getByTestId("terminal"))).toBeCloseTo(term0 + 150, -1);

  // double-click resets to the default
  await page.getByTestId("splitter-left").dblclick();
  await expect.poll(() => width(left)).toBeCloseTo(left0, 0);
});

test("laptop width: inspecting a node hides the requirements column and its handle", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto("/");
  await page.evaluate(() => localStorage.removeItem("kodehub.layout.v1"));
  await page.reload();
  await expect(page.getByTestId("splitter-left")).toBeVisible();
  await page.getByTestId("map-search").fill("verify_cred");
  await page.getByTestId("search-result-verify_credentials").click();
  await expect(page.getByTestId("node-panel")).toBeVisible();
  await expect(page.getByTestId("splitter-left")).toBeHidden();
  await expect(page.getByTestId("splitter-panel")).toBeVisible();
  await page.getByTestId("splitter-panel").hover();
  await page.waitForTimeout(400);
  await page.screenshot({ path: path.join(shots, "panes-laptop.png") });
});

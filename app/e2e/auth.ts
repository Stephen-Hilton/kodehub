// Shared e2e sign-in: the first run creates the account (first user = system admin), later runs
// sign in. The session goes into localStorage before the app loads.
import type { APIRequestContext, Page } from "@playwright/test";

const USER = { username: "demo", password: "password123", display_name: "Demo" };

export async function sessionToken(request: APIRequestContext): Promise<string> {
  const login = await request.post("/api/auth/login", { data: USER });
  if (login.ok()) return (await login.json()).token;
  const reg = await request.post("/api/auth/register", { data: USER });
  if (!reg.ok()) throw new Error(`cannot register e2e user: ${reg.status()} ${await reg.text()}`);
  return (await reg.json()).token;
}

/** Sign the page in and return headers for direct API calls. */
export async function signIn(page: Page, request: APIRequestContext): Promise<Record<string, string>> {
  const token = await sessionToken(request);
  await page.addInitScript((t) => localStorage.setItem("kodehub.session", t), token);
  return { authorization: `Bearer ${token}` };
}

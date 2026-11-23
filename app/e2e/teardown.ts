// Drop the throwaway e2e database.
export default async function teardown() {
  const db = process.env.KODEHUB_E2E_DB;
  if (!db || !db.startsWith("kodehub_e2e_")) return;
  const url = process.env.KODEHUB_ARANGO_URL ?? "http://127.0.0.1:8528";
  const auth = "Basic " + Buffer.from(`root:${process.env.KODEHUB_ARANGO_PASSWORD ?? "kodehub"}`).toString("base64");
  await fetch(`${url}/_db/_system/_api/database/${db}`, { method: "DELETE", headers: { authorization: auth } }).catch(() => {});
}

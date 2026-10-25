// Supplemental, lossless archive for fields/rows the semantic importer does not carry.
// Keep the original semantic import unchanged; archives never enact source deletes or sync work.
import { Database } from "bun:sqlite";
import { createHash } from "node:crypto";
import { isDeepStrictEqual, parseArgs } from "node:util";

type Call = (method: string, path: string, body?: unknown) => Promise<Record<string, any>>;
const digest = (value: string) => createHash("sha256").update(value).digest("hex");
const scope = "project:engram-archive";
const envelope = "engram-archive-v1\n";
export function decodeArchiveChunk(text: string) {
  if (!text.startsWith(envelope)) throw new Error("unsupported archive chunk format");
  return text.slice(envelope.length);
}
const quoted = (name: string) => '"' + name.replaceAll('"', '""') + '"';
const json = (row: Record<string, unknown>) => JSON.stringify(Object.fromEntries(Object.entries(row).map(([k,v]) => [k, v instanceof Uint8Array ? { $blob: Buffer.from(v).toString("base64") } : v])));

export async function archiveSource(path: string, call: Call) {
  if ((await call("GET", "/v1/whoami")).principal !== "personal-import") throw new Error("dedicated import principal required");
  const db = new Database(path, { readonly: true, safeIntegers: true });
  const counts: Record<string, number> = {};
  const manifests: Record<string, string> = {};
  let rows = 0, parts = 0, created = 0;
  try {
    db.exec("PRAGMA query_only=ON; BEGIN");
    const listed = db.query<{ name: string; type: string; schema: string }, []>("PRAGMA table_list").all().filter(t => t.schema === "main" && t.name !== "sqlite_schema");
    const tables = listed.filter(t => t.type === "table").map(t => t.name).sort();
    const excludedDerivedTables = listed.filter(t => t.type !== "table").map(t => t.name).sort();
    for (const permission of ["read", "write"]) await call("POST", "/v1/grants", { principal: "personal-import", scope, permission });
    const put = async (table: string, identity: unknown, record: string, payload: string) => {
      const hash = digest(payload), key = digest(JSON.stringify(["envelope-v1", table, identity, hash]));
      const chars = [...payload], capacity = 80_000 - envelope.length;
      const total = Math.max(1, Math.ceil(chars.length / capacity));
      for (let part = 0; part < total; part++) {
        const text = envelope + chars.slice(part * capacity, (part + 1) * capacity).join("");
        const id = `engram-archive-${key}-${part}`, fullId = `${scope}/${id}`;
        const metadata = { engramArchive: { table, identity, record, sha256: hash, part, total } };
        const route = `/v1/memories/${encodeURIComponent(fullId)}/explain`;
        let stored = await call("GET", route);
        if (stored.missing) {
          const result = await call("POST", "/v1/memories", { scope, id, kind: "engram_archive", text, source: "Engram read-only snapshot archive", metadata, idempotencyKey: id });
          if (result.id !== fullId || result.version !== 1) throw new Error("unexpected archive identity or version");
          created++;
          stored = await call("GET", route);
        }
        const head = stored.versions?.at(-1);
        if (stored.id !== fullId || stored.deleted || stored.currentVersion !== 1 || head?.text !== text || head?.kind !== "engram_archive" || !isDeepStrictEqual(head?.metadata, metadata)) throw new Error("archive verification failed");
        parts++;
      }
    };
    for (const table of tables) {
      const schema = db.query<{ sql: string }, [string]>("select sql from sqlite_master where name=?").get(table)!.sql;
      const columns = db.query<{ name: string; pk: bigint }, []>(`PRAGMA table_info(${quoted(table)})`).all();
      if (columns.some(c => c.name === "__hyperag_archive_rowid")) throw new Error("reserved archive column");
      const pk = columns.filter(c => c.pk > 0).sort((a,b) => Number(a.pk - b.pk)).map(c => c.name);
      // Decimal strings retain 64-bit integers exactly in JSON; column affinity remains in schema.
      const normalize = (row: Record<string, unknown>) => Object.fromEntries(Object.entries(row).map(([k,v]) => [k, typeof v === "bigint" ? (v <= BigInt(Number.MAX_SAFE_INTEGER) && v >= BigInt(Number.MIN_SAFE_INTEGER) ? Number(v) : { $integer: v.toString() }) : v]));
      let count = 0;
      const records: Array<{ identity: unknown; sha256: string }> = [];
      const select = `select ${pk.length ? "" : 'rowid as "__hyperag_archive_rowid", '}* from ${quoted(table)}`;
      for (const raw of db.query<Record<string, unknown>, []>(select).iterate()) {
        const row = normalize(raw), identity = pk.length ? pk.map(k => row[k]) : row.__hyperag_archive_rowid;
        delete row.__hyperag_archive_rowid;
        const payload = json(row);
        await put(table, identity, "row", payload);
        records.push({ identity, sha256: digest(payload) });
        count++; rows++;
      }
      counts[table] = count;
      const manifest = JSON.stringify({ table, schema, primaryKey: pk, records });
      await put(table, "manifest", "manifest", manifest);
      manifests[table] = digest(manifest);
    }
    // The root links every logical row plus original schema, including empty/derived tables.
    const schema = db.query("select type,name,tbl_name,sql from sqlite_master order by type,name").all();
    await put("snapshot", "manifest", "manifest", JSON.stringify({ tables: counts, manifests, schema, excludedDerivedTables }));
    return { tables: counts, rows, parts, created, excludedDerivedTables };
  } finally {
    if (db.inTransaction) db.exec("ROLLBACK");
    db.close();
  }
}

async function main() {
  const { values: args } = parseArgs({ options: { db: { type: "string" }, apply: { type: "boolean" } } });
  if (!args.db || !args.apply || !process.env.HYPERAG_TOKEN) throw new Error("explicit archive apply required");
  const token = process.env.HYPERAG_TOKEN;
  const call: Call = async (method, path, body) => {
    const r = await fetch("http://127.0.0.1:7481" + path, { method, redirect: "error", signal: AbortSignal.timeout(120_000), headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    if (r.status === 404 && method === "GET" && path.endsWith("/explain")) return { missing: true };
    if (!r.ok) throw new Error("archive request failed");
    return await r.json();
  };
  console.log(JSON.stringify(await archiveSource(args.db, call)));
}
if (import.meta.main) {
  try { await main(); }
  catch { console.error("Engram archive failed; source unchanged. Check configuration and counts before retrying."); process.exitCode = 1; }
}

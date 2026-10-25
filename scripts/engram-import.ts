// Read-only Engram source. Default/dry-run selects only counts and ids; --apply needs the
// dedicated memory service on loopback port 7481.
// No backup creation, private files, memory text or server error bodies in output.
import { Database } from "bun:sqlite";
import { createHash } from "node:crypto";
import { isDeepStrictEqual, parseArgs } from "node:util";

type Obs = {
  id: number;
  sync_id: string | null;
  session_id: string;
  type: string;
  title: string;
  content: string;
  tool_name: string | null;
  project: string | null;
  scope: string;
  topic_key: string | null;
  pinned: number;
  created_at: string;
  updated_at: string;
};
type Version = { observation_id: number; title: string | null; content: string; version: number; created_at: string };
type Rel = { source_id: string; target_id: string; relation: string };

export const scopeOf = (o: Pick<Obs, "scope" | "project">) => o.scope === "personal" ? "personal" : `project:${(o.project ?? "none").toLowerCase()}`;
const idOf = (o: Pick<Obs, "sync_id" | "id">) => `engram-${o.sync_id ?? o.id}`;
export const storedId = (o: Obs) => `${scopeOf(o)}/${idOf(o)}`;
const iso = (t: string) => new Date(t.includes("T") ? t : `${t.replace(" ", "T")}Z`).toISOString();
const textOf = (title: string | null, content: string) => title ? `${title}\n${content}` : content;
const relationsSQL = "select source_id, target_id, relation from memory_relations where relation in ('supersedes','related','compatible','scoped') and superseded_at is null";

export function readSource(path: string, apply = false) {
  const db = new Database(path, { readonly: true });
  try {
    db.exec("PRAGMA query_only=ON; BEGIN");
    // No titles, content, project names or metadata are read by a dry run.
    const counts = db.query(`select
      (select count(*) from observations where deleted_at is null) as memories,
      (select count(distinct case when scope='personal' then 'personal' else 'project:' || lower(coalesce(project,'none')) end) from observations where deleted_at is null) as scopes,
      (select count(*) from observation_versions v join observations o on o.id=v.observation_id where o.deleted_at is null) as priorVersions,
      (select count(*) from (${relationsSQL})) as judgedRelations`).get();
    const ids = db.query<{ id: number }, []>("select id from observations where deleted_at is null order by id limit 20").all().map(o => o.id);
    if (!apply) return { counts, ids };
    const observations = db.query<Obs, []>("select * from observations where deleted_at is null order by id").all();
    const versions = db.query<Version, []>("select v.* from observation_versions v join observations o on o.id=v.observation_id where o.deleted_at is null order by observation_id, version").all();
    const relations = db.query<Rel, []>(relationsSQL).all();
    const deletedIds = db.query<{ id: number }, []>("select id from observations where deleted_at is not null order by id").all().map(o => o.id);
    return { counts, ids, observations, versions, relations, deletedIds };
  } finally {
    if (db.inTransaction) db.exec("ROLLBACK");
    db.close();
  }
}

type Call = (method: string, path: string, body?: unknown) => Promise<Record<string, unknown>>;
type StoredVersion = { version: number; text: string; source: string; kind: string; validFrom: string | null; metadata: { engram?: Record<string, unknown> } };
type ImportBody = { text: string; source: string; validFrom: string; metadata: { engram: Record<string, unknown> }; idempotencyKey: string };
export async function importSource(snapshot: ReturnType<typeof readSource>, call: Call) {
  const obs = snapshot.observations!;
  const history = new Map<number, Version[]>();
  for (const v of snapshot.versions!) {
    const prior = history.get(v.observation_id) ?? [];
    prior.push(v);
    history.set(v.observation_id, prior);
  }
  // Validate every date before target writes; errors never include source values.
  for (const o of obs) { iso(o.created_at); iso(o.updated_at); }
  for (const v of snapshot.versions!) iso(v.created_at);
  const who = (await call("GET", "/v1/whoami")).principal as string;
  if (who !== "personal-import") throw new Error("dedicated import principal required");
  for (const scope of new Set(obs.map(scopeOf)))
    for (const permission of ["read", "write"])
      await call("POST", "/v1/grants", { principal: who, scope, permission });

  let updates = 0, added = 0, changed = 0, unchanged = 0;
  const expectedVersions = new Map<string, number>();
  const expectedKinds = new Map<string, string>();
  for (const o of obs) {
    const prior = history.get(o.id) ?? [];
    const states = [...prior, { title: o.title, content: o.content, created_at: o.updated_at }];
    // Snapshot-specific keys make an interrupted import replayable without duplicating history.
    const key = createHash("sha256").update(JSON.stringify([o, prior])).digest("hex");
    const bodies: ImportBody[] = states.map((v, i) => ({
      text: textOf(v.title, v.content), source: `engram session ${o.session_id}`,
      validFrom: iso(i === 0 ? o.created_at : v.created_at),
      metadata: { engram: { id: o.id, sync_id: o.sync_id, project: o.project, topic_key: o.topic_key, created_at: o.created_at, updated_at: o.updated_at, version_created_at: v.created_at, version_ordinal: i + 1, source_version: "version" in v ? v.version : null, tool_name: o.tool_name, pinned: o.pinned === 1 } },
      idempotencyKey: `engram:${key}:${i}`,
    }));
    const id = storedId(o), path = `/v1/memories/${encodeURIComponent(id)}`;
    const existing = await call("GET", `${path}/explain`);
    const versions = (existing.versions ?? []) as StoredVersion[];
    const head = versions.at(-1), latest = bodies.at(-1)!;
    const same = (v: StoredVersion, b: ImportBody) => v.text === b.text && v.source === b.source && v.validFrom === b.validFrom && isDeepStrictEqual(v.metadata.engram, b.metadata.engram);
    if (!existing.missing && (existing.id !== id || existing.deleted || !head || head.metadata.engram?.id !== o.id || head.metadata.engram?.sync_id !== o.sync_id))
      throw new Error("unexpected existing import identity");
    let current = existing.missing ? 0 : existing.currentVersion as number;
    if (!Number.isInteger(current) || current < 0 || (head && head.version !== current)) throw new Error("unexpected existing import version");
    expectedKinds.set(id, head?.kind ?? o.type);
    // Old imports have no digest: compare every stored source state without modifying them.
    const legacyPrefix = versions.length <= bodies.length && versions.every((v, i) => v.kind === o.type && same(v, bodies[i]!));
    const { snapshot_key, source_type, ...headSource } = head?.metadata.engram ?? {};
    const markedHead = head && snapshot_key === key && source_type === o.type && same({ ...head, metadata: { engram: headSource } }, latest);
    if ((legacyPrefix && versions.length === bodies.length) || markedHead) {
      unchanged++;
      expectedVersions.set(id, current);
      continue;
    }
    if (!head) added++; else changed++;
    const put = async (body: ImportBody, keySuffix?: string) => {
      const r = current === 0
        ? await call("POST", "/v1/memories", { ...body, scope: scopeOf(o), id: idOf(o), kind: o.type })
        : await call("PATCH", path, { ...body, ...(keySuffix ? { idempotencyKey: `engram:${key}:catchup:${keySuffix}` } : {}), expectedVersion: current });
      if (r.id !== id || r.version !== current + 1) throw new Error("unexpected import identity or version");
      if (current > 0) updates++;
      current++;
    };
    if (legacyPrefix) {
      // Also resume an interrupted initial import, retaining its original replay keys.
      for (const body of bodies.slice(versions.length)) await put(body);
    } else {
      // Preserve new/revised source history without replaying the old snapshot over the head.
      for (let i = 0; i < prior.length; i++) {
        const body = bodies[i]!;
        if (!versions.some(v => v.text === body.text && v.source === body.source && v.validFrom === body.validFrom
            && v.metadata.engram?.source_version === body.metadata.engram.source_version
            && v.metadata.engram?.version_created_at === body.metadata.engram.version_created_at))
          await put(body, `history:${i}`);
      }
      await put({ ...latest, metadata: { engram: { ...latest.metadata.engram, snapshot_key: key, source_type: o.type } } }, "current");
    }
    expectedVersions.set(id, current);
  }
  const bySync = new Map(obs.filter(o => o.sync_id).map(o => [o.sync_id!, storedId(o)]));
  let linked = 0, skipped = 0;
  for (const r of snapshot.relations!) {
    const from = bySync.get(r.source_id), to = bySync.get(r.target_id);
    if (!from || !to || from === to) { skipped++; continue; }
    await call("POST", "/v1/relations", { from, to, type: r.relation.toUpperCase() });
    linked++;
  }
  let verified = 0;
  for (const o of obs) {
    const e = await call("GET", `/v1/memories/${encodeURIComponent(storedId(o))}/explain`);
    const versions = e.versions as Array<{ text: string; kind: string; metadata: { engram?: { id: number } } }> | undefined;
    if (versions && versions.length === expectedVersions.get(storedId(o))
        && versions.at(-1)?.text === textOf(o.title, o.content)
        && versions.at(-1)?.kind === expectedKinds.get(storedId(o)) && versions.at(-1)?.metadata?.engram?.id === o.id) verified++;
  }
  if (verified !== obs.length) throw new Error("import verification failed");
  return { memories: verified, added, changed, unchanged, updates, relations: linked, skippedRelations: skipped, deletedInSource: snapshot.deletedIds?.length ?? 0 };
}

export async function main(args = process.argv.slice(2)) {
  const { values: a } = parseArgs({ args, options: {
    db: { type: "string" }, url: { type: "string", default: "http://127.0.0.1:7481" },
    "dry-run": { type: "boolean" }, apply: { type: "boolean" },
  } });
  if (!a.db || (a.apply && a["dry-run"])) throw new Error("use --db PATH [--dry-run | --apply]");
  if (a.url !== "http://127.0.0.1:7481") throw new Error("dedicated loopback memory service required");
  const token = process.env.HYPERAG_TOKEN;
  if (a.apply && !token) throw new Error("HYPERAG_TOKEN required for apply");
  const snapshot = readSource(a.db, !!a.apply);
  console.log(JSON.stringify({ counts: snapshot.counts, ids: snapshot.ids }));
  if (!a.apply) return;
  const call: Call = async (method, path, body) => {
    const r = await fetch(a.url + path, {
      method, redirect: "error", signal: AbortSignal.timeout(120_000),
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    // Do not surface a service response: it can contain private source fields.
    if (r.status === 404 && method === "GET" && path.endsWith("/explain")) return { missing: true };
    if (!r.ok) throw new Error("import request failed");
    return await r.json() as Record<string, unknown>;
  };
  console.log(JSON.stringify({ counts: await importSource(snapshot, call), deletedIds: snapshot.deletedIds?.slice(0, 20) }));
}

if (import.meta.main) {
  try { await main(); }
  catch { console.error("Engram import failed; source unchanged. Check configuration and counts before retrying."); process.exitCode = 1; }
}

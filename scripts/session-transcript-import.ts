// Local-only importer. Real transcript files must never be added to this repository.
import { readFile } from "node:fs/promises";
import { Database } from "bun:sqlite";
import { parseArgs, isDeepStrictEqual } from "node:util";
import { hash, parseTranscriptSessions, redact, type Source } from "./session-transcript";
import { storedId } from "./engram-import";

type Call = (method: string, path: string, body?: unknown) => Promise<Record<string, any>>;
type Input = { source: Source; path: string };
export async function importTranscripts(inputs: Input[], snapshot: string, call: Call) {
  if ((await call("GET", "/v1/whoami")).principal !== "personal-import") throw new Error("dedicated import principal required");
  const ownership = new Map<string, { project: string; memories: string[] }>();
  const db = new Database(snapshot, { readonly: true });
  try {
    db.exec("PRAGMA query_only=ON; BEGIN");
    for (const s of db.query<{ id: string; project: string }, []>("select id,project from sessions").iterate()) ownership.set(s.id, { project: s.project, memories: [] });
    for (const o of db.query<any, []>("select * from observations where deleted_at is null").iterate()) ownership.get(o.session_id)?.memories.push(storedId(o));
  } finally { if (db.inTransaction) db.exec("ROLLBACK"); db.close(); }
  const counters: Record<string, { files: number; sessions: number; prompts: number; created: number; updated: number; unchanged: number; redactedProjectLabels: number; skipped: Record<string, number> }> = {};
  const sessions = new Map<string, { source: Source; parsed: ReturnType<typeof parseTranscriptSessions>[number]; files: string[] }>();
  for (const input of inputs) {
    const c = counters[input.source] ??= { files: 0, sessions: 0, prompts: 0, created: 0, updated: 0, unchanged: 0, redactedProjectLabels: 0, skipped: {} };
    c.files++;
    for (const p of parseTranscriptSessions(input.source, await readFile(input.path, "utf8"))) {
      for (const [why,n] of Object.entries(p.skipped)) c.skipped[why] = (c.skipped[why] ?? 0) + n;
      if (!p.session.id) { c.skipped.missingSessionFragment = (c.skipped.missingSessionFragment ?? 0) + 1; continue; }
      const key = `${input.source}:${p.session.id}`, old = sessions.get(key);
      if (!old) sessions.set(key, { source: input.source, parsed: p, files: [hash(input.path)] });
      else {
        old.files.push(hash(input.path)); old.parsed.prompts.push(...p.prompts);
        const a = old.parsed.session, b = p.session;
        a.cwds = [...new Set([...a.cwds, ...b.cwds])].sort();
        a.cwd ??= b.cwd; a.model = b.model ?? a.model;
        for (const k of ["startedAt", "firstEventAt", "lastEventAt", "endedAt"] as const) if (b[k] && (!a[k] || ((k === "startedAt" || k === "firstEventAt") ? Date.parse(b[k]!) < Date.parse(a[k]!) : Date.parse(b[k]!) > Date.parse(a[k]!)))) a[k] = b[k];
      }
    }
  }
  const granted = new Set<string>();
  for (const [key, { source, parsed: p, files }] of sessions) {
    const s = p.session, c = counters[source]!, owner = ownership.get(s.id!);
    if (!owner?.project && !s.cwd) { c.skipped.missingProjectOrCwd = (c.skipped.missingProjectOrCwd ?? 0) + 1; continue; }
    const unsafeProject = !!owner?.project && redact(owner.project) !== owner.project;
    if (unsafeProject) c.redactedProjectLabels++;
    const scope = owner?.project ? `project:${unsafeProject ? "label-" + hash(owner.project.toLowerCase()) : owner.project.toLowerCase()}` : `project:cwd-${hash(s.cwd!)}`;
    if (!granted.has(scope)) {
      for (const permission of ["read", "write"]) await call("POST", "/v1/grants", { principal: "personal-import", scope, permission });
      granted.add(scope);
    }
    const save = async (identity: string, text: string, kind: string, metadata: object) => {
      const id = `${scope}/transcript-${hash(identity)}`, route = `/v1/memories/${encodeURIComponent(id)}`;
      const old = await call("GET", `${route}/explain`), head = old.versions?.at(-1);
      if (!old.missing && (old.id !== id || old.deleted || head?.kind !== kind)) throw new Error("unexpected transcript identity");
      const body = { text: redact(text), source: "Local redacted session transcript", metadata, idempotencyKey: hash(JSON.stringify([identity, text, metadata])) };
      if (head?.text === body.text && isDeepStrictEqual(head?.metadata, metadata)) { c.unchanged++; return id; }
      const result = old.missing ? await call("POST", "/v1/memories", { ...body, scope, id: id.slice(scope.length + 1), kind }) : await call("PATCH", route, { ...body, expectedVersion: old.currentVersion });
      if (result.id !== id || result.version !== (old.missing ? 1 : old.currentVersion + 1)) throw new Error("transcript write identity mismatch");
      const verified = await call("GET", `${route}/explain`), last = verified.versions?.at(-1);
      if (verified.id !== id || verified.currentVersion !== result.version || last?.kind !== kind || verified.deleted || last?.text !== body.text || !isDeepStrictEqual(last?.metadata, metadata)) throw new Error("transcript readback mismatch");
      if (old.missing) c.created++; else c.updated++;
      return id;
    };
    const session = { ...s, id: redact(s.id!), cwds: s.cwds.map(redact), cwd: s.cwd ? redact(s.cwd) : null, model: s.model ? redact(s.model) : null, project: owner?.project ? redact(owner.project) : null };
    const sessionId = await save(key, `Session ${source}\n${JSON.stringify(session)}`, "session", { transcript: { source, ...session, sourceFiles: [...new Set(files)].sort() } });
    c.sessions++;
    if (unsafeProject) c.skipped.unsafeProjectLink = (c.skipped.unsafeProjectLink ?? 0) + (owner?.memories.length ?? 0);
    for (const id of unsafeProject ? [] : owner?.memories ?? []) await call("POST", "/v1/relations", { from: sessionId, to: id, type: "RELATED" });
    const seen = new Set<string>();
    for (const prompt of p.prompts) {
      const identity = `${key}:prompt:${hash(JSON.stringify(prompt))}`;
      if (seen.has(identity)) { c.skipped.duplicateFilePrompt = (c.skipped.duplicateFilePrompt ?? 0) + 1; continue; }
      seen.add(identity);
      const id = await save(identity, `User prompt\n${prompt.text}`, "user_prompt", { transcript: { source, sessionId: session.id, timestamp: prompt.timestamp, project: session.project, cwd: session.cwd } });
      await call("POST", "/v1/relations", { from: id, to: sessionId, type: "RELATED" });
      c.prompts++;
    }
  }
  return counters;
}

async function main() {
  const { values: a } = parseArgs({ options: { db: { type: "string" }, source: { type: "string", multiple: true }, apply: { type: "boolean" } } });
  if (!a.db || !a.apply || !a.source?.length || !process.env.HYPERAG_TOKEN) throw new Error("explicit local transcript import required");
  const inputs: Input[] = [];
  for (const entry of a.source) {
    const split = entry.indexOf(":"), source = entry.slice(0, split), root = entry.slice(split + 1);
    if (!["claude", "codex"].includes(source) || !root.startsWith("/")) throw new Error("invalid source root");
    for await (const path of new Bun.Glob("**/*.jsonl").scan({ cwd: root, absolute: true, followSymlinks: false })) inputs.push({ source: source as Source, path });
  }
  inputs.sort((a,b) => a.path.localeCompare(b.path));
  const token = process.env.HYPERAG_TOKEN;
  const call: Call = async (method, path, body) => {
    const r = await fetch("http://127.0.0.1:7481" + path, { method, redirect: "error", signal: AbortSignal.timeout(120_000), headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    if (r.status === 404 && method === "GET" && path.endsWith("/explain")) return { missing: true };
    if (!r.ok) throw new Error("transcript request failed");
    return await r.json();
  };
  console.log(JSON.stringify(await importTranscripts(inputs, a.db, call)));
}
if (import.meta.main) {
  try { await main(); }
  catch { console.error("Local transcript import failed; sources unchanged. Check configuration and counts before retrying."); process.exitCode = 1; }
}

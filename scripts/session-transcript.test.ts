import { test, expect } from "bun:test";
import { parseTranscript, redact } from "./session-transcript";
const jsonl = (rows: unknown[]) => rows.map(r => JSON.stringify(r)).join("\n");

test("Codex extracts user prompts once, carries session metadata, skips tool/file dumps", () => {
  const p = parseTranscript("codex", jsonl([
    { type: "session_meta", timestamp: "2026-01-01T00:00:00Z", payload: { id: "s1", cwd: "/demo/project" } },
    { type: "turn_context", payload: { model: "synthetic-model" } },
    { type: "event_msg", timestamp: "2026-01-01T00:00:01Z", payload: { type: "user_message", message: "Remember the synthetic rule." } },
    { type: "response_item", timestamp: "2026-01-01T00:00:01Z", payload: { type: "message", role: "user", content: [{ type: "input_text", text: "Remember the synthetic rule." }] } },
    { type: "response_item", payload: { type: "function_call_output", output: "private tool output" } },
    { type: "response_item", payload: { type: "message", role: "user", content: [{ type: "input_text", text: "x".repeat(20_000) }] } },
  ]));
  expect(p.session).toMatchObject({ id: "s1", cwd: "/demo/project", startedAt: "2026-01-01T00:00:00Z", endedAt: null, model: "synthetic-model" });
  expect(p.prompts.map(p => p.text)).toEqual(["Remember the synthetic rule."]);
  expect(p.skipped.largePrompt).toBe(1);
});

test("Claude accepts text blocks, ignores tool results and generated context", () => {
  const p = parseTranscript("claude", jsonl([
    { type: "user", sessionId: "s2", cwd: "/demo/project", timestamp: "2026-01-02T00:00:00Z", message: { role: "user", content: [{ type: "text", text: "Remember this other synthetic rule." }, { type: "tool_result", content: "private tool output" }] } },
    { type: "assistant", sessionId: "s2", message: { model: "synthetic-model" } },
    { type: "user", sessionId: "s2", isMeta: true, message: { content: "generated context" } },
    { type: "user", sessionId: "s2", message: { content: "# AGENTS.md instructions\nprivate injected text" } },
  ]));
  expect(p.session.id).toBe("s2");
  expect(p.session.endedAt).toBeNull();
  expect(p.prompts).toHaveLength(1);
  expect(p.prompts[0]!.text).toBe("Remember this other synthetic rule.");
});

test("redacts credentials, phones, environment dumps and process arguments before returning text", () => {
  const input = 'API_KEY="synthetic-secret-value"\nAuthorization: Bearer synthetic-token\nphone +1 (415) 555-0123\nprocess args: curl -H private\nexport PASSWORD=private\n' + 'sk-' + 'a1'.repeat(24) + '\n-----BEGIN PRIVATE KEY-----\nsynthetic-key\n-----END PRIVATE KEY-----';
  const output = redact(input);
  for (const secret of ["synthetic-secret-value", "synthetic-token", "555-0123", "curl -H", "PASSWORD=private", "synthetic-key", "a1".repeat(24)]) expect(output).not.toContain(secret);
  expect(output).toContain("REDACTED");
});

test("malformed records and missing source session ids are explicit skips, never invented", () => {
  const p = parseTranscript("codex", '{broken}\n' + jsonl([{ type: "event_msg", payload: { type: "user_message", message: "Synthetic text" } }]));
  expect(p.skipped.invalidJson).toBe(1);
  expect(p.session.id).toBeNull();
});

import { Database } from "bun:sqlite";
import { mkdtempSync, writeFileSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { importTranscripts } from "./session-transcript-import";

test("imports redacted prompts and sessions in mapped scopes, links memories and replays without memory writes", async () => {
  const dir = mkdtempSync(join(tmpdir(), "transcript-import-")), dbPath = join(dir,"snapshot.db"), path = join(dir,"source.jsonl");
  const db = new Database(dbPath);
  db.exec("create table sessions(id text,project text); create table observations(id integer,sync_id text,session_id text,scope text,project text,deleted_at text); insert into sessions values ('s1','Demo'); insert into observations values (1,'memory-one','s1','project','Demo',null)"); db.close();
  writeFileSync(path, jsonl([{ type: "session_meta", timestamp:"2026-01-01T00:00:00Z",payload:{id:"s1",cwd:"/synthetic/demo"}}, {type:"response_item",timestamp:"2026-01-01T00:00:01Z",payload:{type:"message",role:"user",content:[{type:"input_text",text:"Remember API_KEY=synthetic-private-value"}]}}]));
  const before = readFileSync(path), beforeDb = readFileSync(dbPath), memories = new Map<string,any>(), edges:any[]=[];let writes=0;
  const call = async (method:string,route:string,body?:any):Promise<any> => {
    if(route==="/v1/whoami")return{principal:"personal-import"};
    if(route==="/v1/grants")return{};
    if(route==="/v1/relations"){edges.push(body);return{};}
    if(method==="GET")return memories.get(decodeURIComponent(route.slice(13,-8)))??{missing:true};
    expect(JSON.stringify(body)).not.toContain("synthetic-private-value");
    expect(body.text.trim().length).toBeGreaterThan(0);
    const id=method==="POST"?`${body.scope}/${body.id}`:decodeURIComponent(route.slice(13));
    const old=memories.get(id),version=(old?.currentVersion??0)+1;
    if(method==="PATCH")expect(body.expectedVersion).toBe(old.currentVersion);
    memories.set(id,{id,currentVersion:version,versions:[...(old?.versions??[]),{...body,kind:old?.versions[0].kind??body.kind,version}]});writes++;return{id,version};
  };
  try{
    const input=[{source:"codex" as const,path}];
    const result=await importTranscripts(input,dbPath,call);
    expect(result.codex).toMatchObject({files:1,sessions:1,prompts:1,created:2});
    expect([...memories.keys()].every(id=>id.startsWith("project:demo/"))).toBe(true);
    expect(edges.some(e=>e.to==="project:demo/engram-memory-one")).toBe(true);
    expect((await importTranscripts(input,dbPath,call)).codex?.unchanged).toBe(2);
    expect(writes).toBe(2);expect(readFileSync(path)).toEqual(before);expect(readFileSync(dbPath)).toEqual(beforeDb);
  }finally{rmSync(dir,{recursive:true});}
});

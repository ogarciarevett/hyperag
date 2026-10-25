import { test, expect } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { parseTranscript } from "./session-transcript";
import { importTranscripts } from "./session-transcript-import";
const jsonl = (rows: unknown[]) => rows.map(r=>JSON.stringify(r)).join("\n");

test("P1: structured dumps, flag forms, DB credentials and national phones never reach storage or metadata", async () => {
  const dir=mkdtempSync(join(tmpdir(),"transcript-redaction-")),dbPath=join(dir,"source.db"),path=join(dir,"source.jsonl");
  const db=new Database(dbPath);db.exec("create table sessions(id text,project text); create table observations(id integer,sync_id text,session_id text,scope text,project text,deleted_at text)");db.close();
  const markers=["synthetic-db-pass","synthetic-key-value","synthetic-inline-key","synthetic-spaced-key","synthetic-env-value","synthetic-nested-value","synthetic-url-pass","612345678","02079460958"];
  const prompts=['{"DATABASE_URL":"postgresql://synthetic:synthetic-db-pass@localhost/example"}', '{"argv":["/synthetic/bin/run","--key","synthetic-key-value"]}', 'Use --key=synthetic-inline-key and --key synthetic-spaced-key for this synthetic request.', 'Environment dump:\n{\n "MISC_VALUE": "synthetic-env-value",\n "nested": {"OTHER": "synthetic-nested-value"}\n}', 'Connect to mongodb://demo:synthetic-url-pass@localhost/example', 'Synthetic phone 612345678 or 02079460958'];
  writeFileSync(path,jsonl([{type:"session_meta",payload:{id:"synthetic-session",cwd:"/synthetic/project"}}, {type:"turn_context",payload:{model:'{"argv":["synthetic-key-value"]}'}},...prompts.map((text,i)=>({type:"response_item",timestamp:`2026-01-01T00:00:0${i}Z`,payload:{type:"message",role:"user",content:[{type:"input_text",text}]}}))]));
  const memories=new Map<string,any>();let checked=0;
  const call=async(method:string,route:string,body?:any):Promise<any>=>{
    if(body){const encoded=JSON.stringify(body);for(const marker of markers)expect(encoded).not.toContain(marker);checked++;}
    if(route==="/v1/whoami")return{principal:"personal-import"};if(route==="/v1/grants"||route==="/v1/relations")return{};
    if(method==="GET")return memories.get(decodeURIComponent(route.slice(13,-8)))??{missing:true};
    const id=`${body.scope}/${body.id}`;memories.set(id,{id,currentVersion:1,versions:[{...body,version:1}]});return{id,version:1};
  };
  try{await importTranscripts([{source:"codex",path}],dbPath,call);expect(checked).toBeGreaterThan(2);}finally{rmSync(dir,{recursive:true});}
});

test("P2: a partial transcript has no invented session start", () => {
  const p=parseTranscript("codex",jsonl([{type:"session_meta",payload:{id:"synthetic",cwd:"/demo"}},{type:"event_msg",timestamp:"2026-01-02T00:00:00Z",payload:{type:"user_message",message:"Synthetic partial prompt"}}]));
  expect(p.session.startedAt).toBeNull();
  expect(p.session).toHaveProperty("firstEventAt","2026-01-02T00:00:00Z");
});

test("P2: partly mirrored streams keep unmatched user events", () => {
  const p=parseTranscript("codex",jsonl([{type:"session_meta",payload:{id:"synthetic",cwd:"/demo"}},{type:"event_msg",timestamp:"2026-01-01T00:00:01Z",payload:{type:"user_message",message:"Synthetic A"}},{type:"response_item",timestamp:"2026-01-01T00:00:02Z",payload:{type:"message",role:"user",content:[{type:"input_text",text:"Synthetic A"}]}},{type:"event_msg",timestamp:"2026-01-01T00:00:03Z",payload:{type:"user_message",message:"Synthetic B"}}]));
  expect(p.prompts.map(p=>p.text)).toEqual(["Synthetic A","Synthetic B"]);
  expect(p.skipped.mirroredUserEvent).toBe(1);
});

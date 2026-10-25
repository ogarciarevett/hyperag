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

async function rejectStorageMarkers(prompts: string[], markers: string[], exact = false) {
  const dir=mkdtempSync(join(tmpdir(),"credential-classes-")),dbPath=join(dir,"source.db"),path=join(dir,"source.jsonl");
  const db=new Database(dbPath);db.exec("create table sessions(id text,project text); create table observations(id integer,sync_id text,session_id text,scope text,project text,deleted_at text)");db.close();
  writeFileSync(path,jsonl([{type:"session_meta",payload:{id:"synthetic-classes",cwd:"/demo"}},...prompts.map((text,i)=>({type:"response_item",timestamp:`2026-01-01T00:00:${String(i).padStart(2,'0')}Z`,payload:{type:"message",role:"user",content:[{type:"input_text",text}]}}))]));
  const memories=new Map<string,any>();let writes=0;
  const call=async(method:string,route:string,body?:any):Promise<any>=>{
    if(body)for(const marker of markers)expect(JSON.stringify(body)).not.toContain(marker);
    if(route==="/v1/whoami")return{principal:"personal-import"};if(route==="/v1/grants"||route==="/v1/relations")return{};
    if(method==="GET")return memories.get(decodeURIComponent(route.slice(13,-8)))??{missing:true};
    const id=`${body.scope}/${body.id}`;memories.set(id,{id,currentVersion:1,versions:[{...body,version:1}]});writes++;return{id,version:1};
  };
  try{await importTranscripts([{source:"codex",path}],dbPath,call);expect(writes).toBeGreaterThan(1);if(exact)expect([...memories.values()].filter(m=>m.versions[0].kind==="user_prompt").map(m=>m.versions[0].text)).toEqual(prompts.map(p=>`User prompt\n${p}`));}finally{rmSync(dir,{recursive:true});}
}

test("P1: authorization tails never reach outgoing storage", async () => {
  await rejectStorageMarkers(['Authorization: Bearer synthetic~synthetic-bearer-tail'],['synthetic-bearer-tail']);
});
test("P2: quoted phone string and numeric values never reach outgoing storage", async () => {
  await rejectStorageMarkers(['{"phone":"2125550198"}', '{"PHONE":2125550199}'],['2125550198','2125550199']);
});
test("rule a: credential labels consume punctuation-rich values until whitespace or closing quote", async () => {
  await rejectStorageMarkers(['Basic synthetic~basic-tail!value', 'token: synthetic~token-tail%value', 'password=synthetic~password-tail^value'],['basic-tail','token-tail','password-tail']);
});
test("rule b: structured key names are case-insensitive and redact whole values", async () => {
  const keys=['phone','tel','mobile','password','secret','token','key','auth','credential','cookie','session'];
  const markers=keys.map((_,i)=>`v${i}~tail!punct`);
  await rejectStorageMarkers(keys.flatMap((k,i)=>[JSON.stringify({[k[0]!.toLowerCase()+k.slice(1).toUpperCase()]:markers[i]}),`${k}=${markers[i]}`]),markers);
});
test("rule c: unlabeled mixed alphanumeric tokens of twenty characters are removed", async () => {
  await rejectStorageMarkers(['Remember opaque value AbCdEfGhIj0123456789.'],['AbCdEfGhIj0123456789']);
});

test("ordinary prose with credential words survives storage byte for byte", async () => {
  await rejectStorageMarkers([
    "Please keep this session open and remember the key decision for the next review.",
    "The monkey jumps over the branch while the author writes a note.",
    "The session and token belong in our design notes.",
    "Explain why the key and password matter to this story.",
    "Discuss auth with the team before the next session.",
    "The fields monkey=jumps and author=writes describe the story.",
  ], [], true);
});

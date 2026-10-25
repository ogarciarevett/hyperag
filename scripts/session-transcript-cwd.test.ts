import { test, expect } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { importTranscripts } from "./session-transcript-import";
import { parseTranscript, parseTranscriptSessions } from "./session-transcript";
const jsonl = (rows: unknown[]) => rows.map(r => JSON.stringify(r)).join("\n");

test("one source session retains every observed cwd within a file", () => {
  const p = parseTranscript("codex", jsonl([
    { type: "session_meta", payload: { id: "synthetic", cwd: "/synthetic/first" } },
    { type: "turn_context", payload: { cwd: "/synthetic/second" } },
    { type: "turn_context", payload: { cwd: "/synthetic/first" } },
  ]));
  expect(p.session.cwd).toBe("/synthetic/first");
  expect(p.session).toHaveProperty("cwds", ["/synthetic/first", "/synthetic/second"]);
});

test("same session across cwd changes imports once, preserves redacted cwd metadata and replays", async () => {
  const dir=mkdtempSync(join(tmpdir(),"transcript-cwd-")), snapshot=join(dir,"source.db");
  const db=new Database(snapshot);db.exec("create table sessions(id text,project text); create table observations(id integer,sync_id text,session_id text,scope text,project text,deleted_at text); insert into sessions values ('synthetic','Demo')");db.close();
  const paths=[join(dir,"first.jsonl"),join(dir,"second.jsonl")];
  for(let i=0;i<2;i++)writeFileSync(paths[i]!,jsonl([{type:"session_meta",payload:{id:"synthetic",cwd:i?"/synthetic/password=private-cwd-marker":"/synthetic/first"}},{type:"response_item",timestamp:`2026-01-01T00:00:0${i}Z`,payload:{type:"message",role:"user",content:[{type:"input_text",text:`Synthetic prompt ${i}`}]}}]));
  const originals=paths.map(p=>readFileSync(p)), sourceBytes=readFileSync(snapshot), memories=new Map<string,any>();let writes=0;
  const call=async(method:string,path:string,body?:any):Promise<any>=>{
    if(body)expect(JSON.stringify(body)).not.toContain("private-cwd-marker");
    if(path==="/v1/whoami")return{principal:"personal-import"};if(path==="/v1/grants"||path==="/v1/relations")return{};
    if(method==="GET")return memories.get(decodeURIComponent(path.slice(13,-8)))??{missing:true};
    const id=`${body.scope}/${body.id}`;expect(method).toBe("POST");memories.set(id,{id,currentVersion:1,versions:[{...body,version:1}]});writes++;return{id,version:1};
  };
  try{
    const inputs=paths.map(path=>({source:"codex" as const,path}));
    const result=await importTranscripts(inputs,snapshot,call);
    expect(result.codex).toMatchObject({files:2,sessions:1,prompts:2,created:3});
    const s=[...memories.values()].find(m=>m.versions[0].kind==="session").versions[0];
    expect(s.metadata.transcript.cwd).toBe("/synthetic/first");
    expect(s.metadata.transcript.cwds).toEqual(["/synthetic/first","/synthetic/password=<REDACTED>"]);
    expect((await importTranscripts(inputs,snapshot,call)).codex?.unchanged).toBe(3);
    expect(writes).toBe(3);paths.forEach((p,i)=>expect(readFileSync(p)).toEqual(originals[i]!));expect(readFileSync(snapshot)).toEqual(sourceBytes);
  }finally{rmSync(dir,{recursive:true});}
});

test("Claude interleaving retains explicit ids and unassigned prelude records remain unassigned", () => {
  const p=parseTranscriptSessions("claude",jsonl([
    {type:"user",message:{content:"Synthetic unassigned prompt"}},
    {type:"user",sessionId:"one",cwd:"/synthetic/one",message:{content:"Synthetic one"}},
    {type:"user",sessionId:"two",cwd:"/synthetic/two",message:{content:"Synthetic two"}},
    {type:"user",sessionId:"one",cwd:"/synthetic/changed",message:{content:"Synthetic one again"}},
  ]));
  expect(p.map(p=>p.session.id)).toEqual([null,"one","two"]);
  expect(p[0]!.prompts).toHaveLength(1);
  expect(p[1]!.prompts).toHaveLength(2);
  expect(p[1]!.session.cwds).toEqual(["/synthetic/changed","/synthetic/one"]);
});

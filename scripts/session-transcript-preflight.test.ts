import { test, expect } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { importTranscripts } from "./session-transcript-import";
const jsonl=(rows:unknown[])=>rows.map(r=>JSON.stringify(r)).join("\n");
async function run(rows:unknown[],project?:string){
 const dir=mkdtempSync(join(tmpdir(),"transcript-preflight-")),snapshot=join(dir,"source.db"),path=join(dir,"source.jsonl");
 const db=new Database(snapshot);db.exec("create table sessions(id text,project text); create table observations(id integer,sync_id text,session_id text,scope text,project text,deleted_at text)");
 if(project){db.query("insert into sessions values ('synthetic',?)").run(project);db.query("insert into observations values (1,'synthetic-observation','synthetic','project',?,null)").run(project);}db.close();writeFileSync(path,jsonl(rows));
 const memories=new Map<string,any>();
 const call=async(method:string,route:string,body?:any):Promise<any>=>{
  if(project&&body)expect(JSON.stringify(body)).not.toContain(project);
  if(route==="/v1/whoami")return{principal:"personal-import"};if(route==="/v1/grants"||route==="/v1/relations")return{};
  if(method==="GET")return memories.get(decodeURIComponent(route.slice(13,-8)))??{missing:true};
  const id=`${body.scope}/${body.id}`;expect(method).toBe("POST");memories.set(id,{id,currentVersion:1,versions:[{...body,version:1}]});return{id,version:1};
 };
 try{return{result:await importTranscripts([{source:"codex",path}],snapshot,call),memories:[...memories.values()].map(m=>m.versions[0])};}finally{rmSync(dir,{recursive:true});}
}
const prompt=(text:string)=>({type:"response_item",payload:{type:"message",role:"user",content:[{type:"input_text",text}]}});
test("forked files keep both explicit source session identities and their own prompts",async()=>{
 const {result,memories}=await run([{type:"session_meta",payload:{id:"original",cwd:"/synthetic/one"}},prompt("Synthetic original prompt"),{type:"session_meta",payload:{id:"forked",cwd:"/synthetic/two"}},prompt("Synthetic forked prompt")]);
 expect(result.codex).toMatchObject({files:1,sessions:2,prompts:2,created:4});
 expect(memories.filter(m=>m.kind==="session").map(m=>m.metadata.transcript.id).sort()).toEqual(["forked","original"]);
 expect(memories.filter(m=>m.kind==="user_prompt").map(m=>m.metadata.transcript.sessionId).sort()).toEqual(["forked","original"]);
});
test("redacted project labels use a stable hashed scope, preserve the session and omit unsafe links",async()=>{
 const project="syntheticprojectabc123xyz456ABC";
 const {result,memories}=await run([{type:"session_meta",payload:{id:"synthetic",cwd:"/synthetic/project"}},prompt("Synthetic project prompt")],project);
 expect(result.codex).toMatchObject({files:1,sessions:1,prompts:1,created:2,redactedProjectLabels:1,skipped:{unsafeProjectLink:1}});
 expect(memories.every(m=>m.scope.startsWith("project:label-"))).toBe(true);
});

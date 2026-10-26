import { afterAll,beforeAll,expect,test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { Pool } from "pg";
import { freshDatabase } from "./helpers";

const code=readFileSync(new URL("../crates/hyperag/src/store.rs",import.meta.url),"utf8");
const rankable=code.match(/const RANKABLE: &str = "([\s\S]*?)";/)![1]!;
const expand=(s:string)=>s.replaceAll("{RANKABLE}",rankable).replaceAll("{BM25_K1}","1.2").replaceAll("{BM25_B}","0.75");
const legacy=expand(code.match(/fn bm25_sql\(\) -> String \{\s*format!\(\s*"([\s\S]*?)"\s*\)/)![1]!);
const template=code.match(/const INDEXED_BM25_SQL: &str = r#"([\s\S]*?)"#;/)?.[1];
// Missing implementation uses unchanged legacy SQL to establish the index-use RED.
const indexed=template ? expand(template.replace("{legacy}",legacy)) : legacy;
const db=await freshDatabase();const pool=new Pool({connectionString:db.url,max:1});let closing=false;
pool.on("error",(e:Error&{code?:string})=>{if(!(closing&&e.code==="57P01"))throw e;});
beforeAll(async()=>{
 for(const name of ["0001_init.sql","0002_settings.sql","0003_memory_text_limit.sql"])
  await pool.query(readFileSync(new URL(`../crates/hyperag/migrations/${name}`,import.meta.url),"utf8"));
 await pool.query(`insert into memories(id,scope,kind,created_by,current_version)
   select 'background-'||n,'allowed','fact','fixture',1 from generate_series(1,3000)n;
  insert into memory_versions(memory_id,version,text,source,author,content_hash)
   select id,1,'ordinary background documents','fixture','fixture',id from memories;
  insert into memories(id,scope,kind,created_by,current_version)
   select 'repeated-'||n,'allowed','fact','fixture',1 from generate_series(1,24)n;
  insert into memories(id,scope,kind,created_by,current_version) values
   ('all-terms','allowed','fact','fixture',1),('rare','allowed','fact','fixture',1),('both','allowed','fact','fixture',1),
   ('selective','allowed','fact','fixture',1),('punct','allowed','fact','fixture',1),
   ('future','allowed','fact','fixture',1),('hidden','hidden','fact','fixture',1),('stale','allowed','fact','fixture',2);
  insert into memory_versions(memory_id,version,text,source,author,content_hash)
   select id,current_version,case when id like 'repeated-%' then repeat('ordinary ',20)
     when id='all-terms' then 'ordinary ordinary quartz quartz quartz nebula' when id='rare' then 'quartz' when id='both' then 'quartz nebula quartz' when id='selective' then 'ultrararefixturetoken'
     when id='punct' then '!!!' when id='future' then 'quartz nebula' when id='hidden' then repeat('quartz ',100) else 'neutral current' end,
     'fixture','fixture',id from memories where id not like 'background-%';
  insert into memory_versions(memory_id,version,text,source,author,content_hash)values('stale',1,repeat('quartz ',100),'fixture','fixture','old');
  update memory_versions set valid_from='2030-01-01',valid_to='2040-01-01' where memory_id='future';
  analyze memories; analyze memory_versions`);
 await pool.query(readFileSync(new URL("../crates/hyperag/migrations/0004_lexical_cache.sql",import.meta.url),"utf8"));
 // Upgrade a populated legacy cache, not just an empty database.
 for(let i=0;i<100;i++){if(Number((await pool.query("select lexical_cache_backfill(256)n")).rows[0].n)===0)break;}
 const migration=new URL("../crates/hyperag/migrations/0005_lexical_postings.sql",import.meta.url);
 if(existsSync(migration))await pool.query(readFileSync(migration,"utf8"));
},60000);
afterAll(async()=>{closing=true;await pool.end();await db.drop();});
const compare=async(scopes:string[],at:string|null,q:string)=>{
 const params=[scopes,at,q,24];
 const actual=(await pool.query(indexed,params)).rows,expected=(await pool.query(legacy,params)).rows;
 expect(actual).toEqual(expected);
 return actual;
};
test("unready falls back, then exact legacy scores/order across scopes, dates and rare/common terms",async()=>{
 await compare(["allowed"],null,"ordinary quartz");
 for(let i=0;i<100;i++){if(Number((await pool.query("select lexical_cache_backfill(256)n")).rows[0].n)===0)break;}
 expect((await pool.query("select value from settings where key='lexical_cache_ready'")).rows[0].value).toBe("true");
 if((await pool.query("select to_regprocedure('lexical_postings_backfill(integer)') as fn")).rows[0].fn){
  await pool.query("select lexical_postings_backfill(1)");await compare(["allowed"],null,"ordinary quartz");
  for(let i=0;i<100;i++){if(Number((await pool.query("select lexical_postings_backfill(256)n")).rows[0].n)===0)break;}
 }
 for(const scopes of [["allowed"],["hidden"],["allowed","hidden"],[]])
  for(const at of [null,"2026-01-01","2035-01-01","2040-01-01"])
   for(const q of ["ordinary quartz","ordinary quartz nebula","quartz nebula","'quartz' | nebula","no-synthetic-match"])
    await compare(scopes,at,q);
 const rare=await compare(["allowed"],null,"ordinary quartz");expect(rare.some(r=>r.id==='rare')).toBe(true);
},60000);
test("ready search reads indexed postings with no source-table or whole-document scan",async()=>{
 await pool.query("vacuum (analyze) memory_versions");
 const nodes:any[]=[];const walk=(n:any)=>{nodes.push(n);for(const p of n.Plans??[])walk(p);};
 const plan=(await pool.query(`explain(analyze,buffers,format json) ${indexed}`,[["allowed"],null,"ultrararefixturetoken",24])).rows[0]["QUERY PLAN"][0];walk(plan.Plan);
 expect(nodes.some(n=>n["Index Name"]==='lexical_postings_pkey'&&n["Actual Loops"]>0)).toBe(true);
 expect(nodes.filter(n=>['memory_versions','memories'].includes(n["Relation Name"])).every(n=>n["Actual Loops"]===0)).toBe(true);
 const broad=(await pool.query(`explain(analyze,buffers,format json) ${indexed}`,[["allowed"],null,"ordinary quartz",24])).rows[0]["QUERY PLAN"][0];nodes.length=0;walk(broad.Plan);
 expect(nodes.filter(n=>n["Node Type"]==='Function Scan').every(n=>n["Actual Loops"]<=1)).toBe(true);
 expect(nodes.some(n=>n["Node Type"]==='Function Scan'&&n["Actual Loops"]===1)).toBe(true);
 expect(nodes.filter(n=>['memory_versions','memories'].includes(n["Relation Name"])||(n["Relation Name"]==='lexical_documents'&&n["Node Type"]==='Seq Scan')).every(n=>n["Actual Loops"]===0)).toBe(true);
 expect(nodes.filter(n=>n["Relation Name"]==='lexical_postings'&&n["Node Type"]==='Seq Scan').every(n=>n["Actual Loops"]===0)).toBe(true);
 console.info('indexed-bm25-fixture',JSON.stringify({selectiveMs:plan["Execution Time"],broadMs:broad["Execution Time"]}));
});
test("metadata/postings backfill is idempotent, bounded and matches every current source lexeme",async()=>{
 await pool.query("select lexical_cache_backfill(64)");
 await pool.query("select lexical_postings_backfill(64)");
 expect((await pool.query("select value from settings where key='lexical_postings_ready'")).rows[0].value).toBe("true");
 await postingsOracle();
 const before=(await pool.query("select memory_id,posting_id from lexical_documents order by memory_id")).rows;
 await pool.query("begin; select pg_advisory_xact_lock(1213811052,1)");
 for(const name of ["0004_lexical_cache.sql","0005_lexical_postings.sql"])await pool.query(readFileSync(new URL(`../crates/hyperag/migrations/${name}`,import.meta.url),"utf8"));
 await pool.query("commit; select lexical_refresh_document('both')");
 expect((await pool.query("select memory_id,posting_id from lexical_documents order by memory_id")).rows).toEqual(before);
 await postingsOracle();
 for(const n of [null,0,257])await expect(pool.query("select lexical_postings_backfill($1)",[n])).rejects.toThrow();
});

test("write-maintained statistics preserve equality after mutation and reset fallback",async()=>{
 await pool.query("update memory_versions set text='quartz quartz nebula nebula' where memory_id='both'; update memories set scope='hidden' where id='rare'");
 await compare(["allowed"],null,"ordinary quartz");await compare(["allowed","hidden"],"2035-01-01","quartz nebula");
 await pool.query("insert into memory_versions(memory_id,version,text,source,author,content_hash) values('both',2,'quartz fresh comet','fixture','fixture','new-head'); update memories set current_version=2 where id='both'");
 await compare(["allowed","hidden"],null,"quartz nebula");await postingsOracle();
 await pool.query("update memory_versions set status='invalidated' where memory_id='both'; update memories set deleted_at=now() where id='future'");
 await compare(["allowed"],null,"quartz nebula");
 await postingsOracle();
 // Prove reset/missing-marker fallback with positive source hits and deliberately empty caches.
 await pool.query("begin; select pg_advisory_xact_lock(1213811052,1); update settings set value='reset' where key='lexical_cache_ready'; truncate lexical_documents,lexical_corpus_stats,lexical_term_stats cascade; commit");
 expect((await compare(["allowed","hidden"],null,"quartz nebula")).length).toBeGreaterThan(0);
 await pool.query("delete from settings where key='lexical_cache_ready'");
 await compare(["allowed","hidden"],null,"quartz nebula");
 await pool.query("insert into settings(key,value) values('lexical_cache_ready','reset')");
 await pool.query("begin; select pg_advisory_xact_lock(1213811052,1); truncate memory_versions; commit");await compare(["allowed"],null,"quartz");
});

async function postingsOracle(){
 const actual=(await pool.query("select d.memory_id,d.version,d.current_version,d.status,d.scope,d.recorded_at,p.lexeme,p.tf from lexical_documents d join lexical_postings p using(posting_id) order by d.memory_id,p.lexeme")).rows;
 const expected=(await pool.query("select m.id as memory_id,v.version,m.current_version,v.status,m.scope,v.recorded_at,u.lexeme,coalesce(array_length(u.positions,1),1) as tf from memories m join memory_versions v on v.memory_id=m.id and v.version=m.current_version cross join lateral unnest(v.text_search)u where m.deleted_at is null and v.status='active' order by m.id,u.lexeme")).rows;
 expect(actual.length).toBeGreaterThan(3000);
 expect(actual).toEqual(expected);
}

test("100k broad query reads only indexed term/scope postings",async()=>{
 await pool.query("select lexical_cache_backfill(64); select lexical_postings_backfill(64)");
 // Bulk-load equivalent synthetic source/cache rows so this tests the planner, not backfill throughput.
 await pool.query(`begin; select pg_advisory_xact_lock(1213811052,1);
  alter table memories disable trigger lexical_memory_sync;
  alter table memory_versions disable trigger lexical_version_sync;
  alter table lexical_documents disable trigger lexical_delta;
  alter table lexical_documents disable trigger lexical_postings_sync;
  insert into memories(id,scope,kind,created_by,current_version)
   select 'large-'||n,case when n%7=0 then 'hidden' else 'allowed' end,'fact','fixture',1 from generate_series(1,100000)n;
  insert into memory_versions(memory_id,version,text,source,author,content_hash)
   select id,1,'ordinary quartz common term'||substring(id from 7),'fixture','fixture',id from memories where id like 'large-%';
  insert into lexical_documents(memory_id,version,scope,valid_from,valid_to,doc_len,tf,current_version,status,recorded_at)
   select m.id,v.version,m.scope,'-infinity','infinity',length(v.text_search),
    (select jsonb_object_agg(u.lexeme,coalesce(array_length(u.positions,1),1)) from unnest(v.text_search)u),
    m.current_version,v.status,v.recorded_at from memories m join memory_versions v on v.memory_id=m.id;
  insert into lexical_corpus_stats select scope,valid_from,valid_to,count(*),sum(doc_len) from lexical_documents group by scope,valid_from,valid_to;
  insert into lexical_term_stats select d.scope,k.key,d.valid_from,d.valid_to,count(*) from lexical_documents d cross join lateral jsonb_each(d.tf)k group by d.scope,k.key,d.valid_from,d.valid_to;
  insert into lexical_postings select d.posting_id,k.key,d.scope,k.value::int from lexical_documents d cross join lateral jsonb_each_text(d.tf)k;
  alter table memories enable trigger lexical_memory_sync;
  alter table memory_versions enable trigger lexical_version_sync;
  alter table lexical_documents enable trigger lexical_delta;
  alter table lexical_documents enable trigger lexical_postings_sync; commit`);
 for(const name of ["memories","memory_versions","lexical_documents","lexical_postings","lexical_corpus_stats","lexical_term_stats"])
  await pool.query(`vacuum(analyze) ${name}`);
 expect(Number((await pool.query("select count(*)n from memory_versions")).rows[0].n)).toBe(100000);
 expect(Number((await pool.query("select count(*)n from lexical_postings")).rows[0].n)).toBe(400000);
 await compare(["allowed"],null,"ordinary quartz");
 const plan=(await pool.query(`explain(analyze,buffers,format json) ${indexed}`,[["allowed"],null,"ordinary quartz",24])).rows[0]["QUERY PLAN"][0];
 const nodes:any[]=[];const walk=(n:any)=>{nodes.push(n);for(const c of n.Plans??[])walk(c);};walk(plan.Plan);
 expect(nodes.filter(n=>n["Relation Name"]==='lexical_postings'&&n["Node Type"]==='Seq Scan').every(n=>n["Actual Loops"]===0)).toBe(true);
 const termScans=nodes.filter(n=>n["Index Name"]==='lexical_postings_pkey'&&n["Actual Loops"]>0);
 expect(termScans.length).toBeGreaterThan(0);
 expect(termScans.every(n=>n["Index Cond"]?.includes('lexeme')&&n["Index Cond"]?.includes('scope'))).toBe(true);
 console.info('postings-100k-plan',JSON.stringify({executionMs:plan["Execution Time"],sourceRows:100000,postings:400000}));
},180000);

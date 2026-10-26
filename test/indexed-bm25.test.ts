import { afterAll,beforeAll,expect,test } from "bun:test";
import { readFileSync } from "node:fs";
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
   ('rare','allowed','fact','fixture',1),('both','allowed','fact','fixture',1),
   ('selective','allowed','fact','fixture',1),('punct','allowed','fact','fixture',1),
   ('future','allowed','fact','fixture',1),('hidden','hidden','fact','fixture',1),('stale','allowed','fact','fixture',2);
  insert into memory_versions(memory_id,version,text,source,author,content_hash)
   select id,current_version,case when id like 'repeated-%' then repeat('ordinary ',20)
     when id='rare' then 'quartz' when id='both' then 'quartz nebula quartz' when id='selective' then 'ultrararefixturetoken'
     when id='punct' then '!!!' when id='future' then 'quartz nebula' when id='hidden' then repeat('quartz ',100) else 'neutral current' end,
     'fixture','fixture',id from memories where id not like 'background-%';
  insert into memory_versions(memory_id,version,text,source,author,content_hash)values('stale',1,repeat('quartz ',100),'fixture','fixture','old');
  update memory_versions set valid_from='2030-01-01',valid_to='2040-01-01' where memory_id='future';
  analyze memories; analyze memory_versions`);
 await pool.query(readFileSync(new URL("../crates/hyperag/migrations/0004_lexical_cache.sql",import.meta.url),"utf8"));
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
 for(const scopes of [["allowed"],["hidden"],["allowed","hidden"],[]])
  for(const at of [null,"2026-01-01","2035-01-01","2040-01-01"])
   for(const q of ["ordinary quartz","quartz nebula","'quartz' | nebula","no-synthetic-match"])
    await compare(scopes,at,q);
 const rare=await compare(["allowed"],null,"ordinary quartz");expect(rare.some(r=>r.id==='rare')).toBe(true);
},60000);
test("ready selective search executes GIN and no document-lexeme expansion",async()=>{
 await pool.query("vacuum (analyze) memory_versions");
 const nodes:any[]=[];const walk=(n:any)=>{nodes.push(n);for(const p of n.Plans??[])walk(p);};
 const plan=(await pool.query(`explain(analyze,buffers,format json) ${indexed}`,[["allowed"],null,"ultrararefixturetoken",24])).rows[0]["QUERY PLAN"][0];walk(plan.Plan);
 expect(nodes.some(n=>n["Node Type"]==='Bitmap Index Scan'&&n["Index Name"]==='memory_versions_text_search_idx'&&n["Actual Loops"]>0)).toBe(true);
 const broad=(await pool.query(`explain(analyze,buffers,format json) ${indexed}`,[["allowed"],null,"ordinary quartz",24])).rows[0]["QUERY PLAN"][0];nodes.length=0;walk(broad.Plan);
 expect(nodes.filter(n=>n["Node Type"]==='Function Scan'&&n["Function Name"]==='unnest').every(n=>n["Actual Loops"]<=1)).toBe(true);
 console.info('indexed-bm25-fixture',JSON.stringify({selectiveMs:plan["Execution Time"],broadMs:broad["Execution Time"]}));
});
test("write-maintained statistics preserve equality after mutation and reset fallback",async()=>{
 await pool.query("update memory_versions set text='quartz quartz nebula nebula' where memory_id='both'; update memories set scope='hidden' where id='rare'");
 await compare(["allowed"],null,"ordinary quartz");await compare(["allowed","hidden"],"2035-01-01","quartz nebula");
 await pool.query("update memory_versions set status='invalidated' where memory_id='both'; update memories set deleted_at=now() where id='future'");
 await compare(["allowed"],null,"quartz nebula");
 await pool.query("begin; select pg_advisory_xact_lock(1213811052,1); truncate memory_versions; commit");await compare(["allowed"],null,"quartz");
});

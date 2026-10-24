// Dry run of the GPU-endpoint profiles against a local stub (no GPU, no cost): checks request
// shape, the 1,024-dim truncation and re-normalisation, prefixes and re-rank score mapping.
//   bun eval/public/gpu-stub-check.ts
import factory from "./systems/hyperag";

const seen: Array<{ path: string; body: Record<string, unknown> }> = [];
const stub = Bun.serve({
  port: 0,
  async fetch(req) {
    const path = new URL(req.url).pathname;
    const body = (await req.json()) as Record<string, unknown>;
    seen.push({ path, body });
    if (path === "/v2/embed") {
      const inputs = body.inputs as Array<{ content: Array<{ text: string }> }>;
      // 4,096-d vectors; the word "retry" points along dimension 0, everything else along 1.
      return Response.json({ embeddings: { float: inputs.map((x) => Array.from({ length: 4096 }, (_, d) => (d === (x.content[0]!.text.includes("retry") ? 0 : 1) ? 3 : 0.001))) } });
    }
    if (path === "/classify") {
      const m = body.messages as Array<{ role: string; content: Array<{ text: string }> }>;
      const ok = m[0]?.role === "query" && m[1]?.role === "document" && body.add_special_tokens === false && body.use_activation === false;
      return Response.json({ data: [{ probs: [ok && m[1]!.content[0]!.text.includes("Jobs retry") ? 5 : -5] }] });
    }
    if (path === "/v1/rerank") {
      const docs = body.documents as string[];
      return Response.json({ results: docs.map((d, index) => ({ index, relevance_score: d.includes("retry") ? 5 : -5 })).reverse() });
    }
    return new Response("not found", { status: 404 });
  },
});
process.env.HYPERAG_GPU_URL = stub.url.origin;
let failed = 0;
const check = (what: string, ok: boolean) => {
  console.log(`${ok ? "pass" : "FAIL"}  ${what}`);
  if (!ok) failed++;
};
const s = await factory("nemotron+nvrerank");
const ns = "smoke/nemotron-stub";
await s.open(ns);
await s.ingest(ns, [
  { id: "a", text: "Jobs retry three times." },
  { id: "b", text: "Plants are watered on Mondays." },
]);
const hits = await s.search(ns, "how often do jobs retry?", 2);
check("re-ranked order puts the matching document first", hits[0]?.id === "a");
check("documents use input_type document, without hand-made prefixes", seen.some((x) => x.path === "/v2/embed" && x.body.input_type === "document" && !JSON.stringify(x.body.inputs).includes("passage:")));
check("the query uses input_type query", seen.some((x) => x.path === "/v2/embed" && x.body.input_type === "query"));
check("rerank defaults to /classify with query and document roles", seen.some((x) => x.path === "/classify" && x.body.model === "nvidia/Nemotron-3.5-Rerank-8B-BF16"));
process.env.HYPERAG_GPU_RERANK_API = "rerank";
const viaRerank = await s.search(ns, "how often do jobs retry?", 2);
check("the rerank API gives the same order", viaRerank[0]?.id === "a" && seen.some((x) => x.path === "/v1/rerank"));
const meta = await s.describe();
check("space records the 1,024-dim truncation", (meta.config.embedding as { dimensions: number; model: string }).dimensions === 1024 && String((meta.config.embedding as { model: string }).model).includes("first-1024-dims"));
await s.close();
stub.stop();
console.log(failed === 0 ? "gpu stub check: all passed" : `gpu stub check: ${failed} failed`);
process.exit(failed === 0 ? 0 : 1);

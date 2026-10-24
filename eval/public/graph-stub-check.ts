// Dry run of the `+graph` profile against a stub Neo4j Query API (no Neo4j, no models): checks the
// edges written for a two-session conversation, that a graph hit reaches the results, that a
// failed graph channel is an error and that a corpus writes no edges. Needs the local Postgres.
//   bun eval/public/graph-stub-check.ts
import { Pool } from "pg";

import factory from "./systems/hyperag";

const relations: Array<Record<string, unknown>> = [];
let expandTo = "";
let down = false;
let expansions = 0;
const stub = Bun.serve({
  port: 0,
  async fetch(req) {
    const { statement, parameters } = (await req.json()) as {
      statement: string;
      parameters: Record<string, unknown>;
    };
    if (statement.includes("MERGE (a)-[r:REL")) relations.push(parameters);
    // Expansion returns one neighbour the query's words do not match, as a real graph would.
    if (statement.includes("MATCH p =")) expansions++;
    if (statement.includes("MATCH p =") && down)
      return new Response("down", { status: 503 });
    if (statement.includes("MATCH p ="))
      return Response.json({
        data: {
          values: expandTo
            ? [[expandTo, 1, [(parameters.seeds as string[])[0]]]]
            : [],
        },
      });
    return Response.json({ data: { values: [] } });
  },
});
process.env.HYPERAG_NEO4J_URL = `http://127.0.0.1:${stub.port}`;

const set = `stubgraph${Date.now().toString(36)}`;
const s = await factory("bm25+graph");
const ns = `${set}/conv-1`;
await s.open(ns);
await s.ingest(ns, [
  {
    id: "D1:1",
    speaker: "Caroline",
    text: 'Caroline said, "I adopted a cat named Luna."',
  },
  {
    id: "D1:2",
    speaker: "Melanie",
    text: 'Melanie said, "That is lovely news."',
  },
]);
await s.ingest(ns, [
  {
    id: "D2:1",
    speaker: "Melanie",
    text: 'Melanie said, "How is Luna doing?"',
  },
  {
    id: "D2:2",
    speaker: "Caroline",
    text: 'Caroline said, "She sleeps all day on the sofa."',
  },
]);
const got = relations.map((r) => `${r.from} ${r.type} ${r.to}`);
const want = [
  `${ns}/D1:2 NEXT_TURN ${ns}/D1:1`,
  `${ns}/D2:1 SAME_ENTITY ${ns}/D1:1`,
  `${ns}/D2:2 NEXT_TURN ${ns}/D2:1`,
];
const checks: Array<[string, boolean]> = [
  [
    `edges ${JSON.stringify(got)}`,
    JSON.stringify(got) === JSON.stringify(want),
  ],
  [
    "edges carry the conversation scope",
    relations.every((r) => r.fromScope === ns && r.toScope === ns),
  ],
];
expandTo = `${ns}/D2:2`;
const hits = await s.search(ns, "adopted cat", 10);
checks.push([
  `graph neighbour D2:2 in results ${JSON.stringify(hits.map((h) => h.id))}`,
  hits.some((h) => h.id === "D2:2"),
]);
down = true;
{
  const err = await s.search(ns, "adopted cat", 10).then(
    () => "",
    (e) => String(e),
  );
  checks.push([
    `a failed graph channel is an error (${err.slice(0, 60)})`,
    err.includes("graph channel failed"),
  ]);
}
// A corpus (BEIR) runs with the graph channel on and writes no edges: it has no relations.
const before = relations.length;
const expansionsBeforeCorpus = expansions;
down = false;
expandTo = "";
await s.ingest(`${set}corpus`, [{ id: "d1", text: "a document" }]);
const corpusHits = await s.search(`${set}corpus`, "document", 10);
checks.push([
  "a corpus expands the graph, writes no edges and still answers",
  relations.length === before && expansions > expansionsBeforeCorpus && corpusHits.length === 1,
]);
await s.close();
stub.stop();

const PG =
  process.env.HYPERAG_PG_URL ??
  "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";
const admin = new Pool({ connectionString: PG, max: 1 });
for (const db of [`pub_${set}_bm25`, `pub_${set}corpus_bm25`])
  await admin.query(`drop database if exists ${db}`);
await admin.end();

for (const [name, ok] of checks) console.log(`${ok ? "ok  " : "FAIL"} ${name}`);
const failed = checks.filter(([, ok]) => !ok).length;
console.log(`${checks.length - failed}/${checks.length}`);
process.exit(failed ? 1 : 0);

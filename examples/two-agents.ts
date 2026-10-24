// Two agents share one team scope. The researcher records evidence and a finding linked to it;
// the fixer retrieves both, a correction supersedes a fact, a restricted scope stays hidden, and an
// unrelated question reports insufficient evidence.
// Run: docker compose -p hyperag up -d && bun examples/two-agents.ts
import { Pool } from "pg";

import { createLocalEmbedder, createNeo4jGraph, createPostgresStore, Hyperag } from "../src";

const pool = new Pool({ connectionString: process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag" });
const h = new Hyperag({
  store: createPostgresStore(pool),
  admins: ["admin"],
  embedder: createLocalEmbedder(),
  graph: createNeo4jGraph({ baseUrl: "http://127.0.0.1:57474", username: "neo4j", password: "fake-test-only" }),
});
await h.init();

const team = "example:payments-team";
const secret = "example:security-private";
for (const agent of ["researcher", "fixer"]) {
  await h.grant("admin", agent, team, "read");
  await h.grant("admin", agent, team, "write");
}
await h.grant("admin", "researcher", secret, "read");
await h.grant("admin", "researcher", secret, "write");

const evidence = await h.ingest("researcher", {
  scope: team, kind: "evidence", source: "incident INC-311 log",
  text: "INC-311: refunds failed between 02:10 and 02:40 because the webhook signing key had rotated.",
});
const finding = await h.ingest("researcher", {
  scope: team, kind: "finding", source: "researcher analysis",
  text: "Finding: reload the webhook signing key on rotation; the refund worker caches it forever.",
});
await h.relate("researcher", { from: finding.id, to: evidence.id, type: "SUPPORTED_BY" });
const oldOwner = await h.ingest("researcher", { scope: team, source: "team wiki", text: "The refund worker is owned by team Atlas." });
await h.ingest("researcher", { scope: secret, source: "pentest", text: "Pentest note: the admin panel accepts weak passwords." });

const handoff = await h.context("fixer", "why did refunds fail and what is the fix?", { maxTokens: 400 });
console.log("fixer context:\n" + handoff.text, "\n", handoff.citations);

await h.ingest("fixer", {
  scope: team, source: "org chart 2026-10", text: "Correction: the refund worker is owned by team Borealis.",
  supersedes: [oldOwner.id],
});
const owner = await h.retrieve("fixer", "who owns the refund worker?");
console.log("owner now:", owner.items[0]?.memory.text);
console.log("old fact history:", (await h.explain("fixer", oldOwner.id)).versions.map((v) => [v.status, v.statusReason]));

const restricted = await h.retrieve("fixer", "admin panel weak passwords pentest");
console.log("fixer sees security scope:", restricted.items.some((i) => i.memory.scope === secret));

const unknown = await h.context("fixer", "what is the capital of Mongolia?");
console.log("unrelated question:", unknown.retrieval.insufficientEvidence, unknown.text);
await pool.end();

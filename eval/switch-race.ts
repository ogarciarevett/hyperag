// A model switch while another agent keeps writing: afterwards the new embedding index must hold a
// vector for every memory, and the old index must still be there.
//   HYPERAG_SERVICE_CMD="target/release/hyperag serve" bun eval/switch-race.ts
import { Pool } from "pg";

import { startService } from "./hyperag-http";

const PG = process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";
const FROM = "hf:Qdrant/bge-small-en-v1.5-onnx-Q";
const TO = "hf:Xenova/multilingual-e5-small?revision=761b726dd34fb83930e26aab4e9ac3899aa1fa78&dtype=q8&pooling=mean";
const name = `${process.env.HYPERAG_DB_PREFIX ?? ""}switch_race_${Date.now()}`;
const admin = new Pool({ connectionString: PG, max: 1 });
await admin.query(`create database ${name}`);
const svc = await startService({ pgUrl: PG.replace(/\/[^/]+$/u, `/${name}`), principals: ["admin", "writer"], admins: ["admin"], embedding: FROM, reranker: "none" });
let ok = true;
try {
  await svc.must("admin", "POST", "/v1/grants", { principal: "writer", scope: "s", permission: "write" });
  for (let i = 0; i < 100; i++) await svc.must("writer", "POST", "/v1/memories", { scope: "s", source: "before", text: `Fact ${i} written before the switch.` });
  let written = 100;
  let switching = true;
  const writer = (async () => {
    while (switching) {
      await svc.must("writer", "POST", "/v1/memories", { scope: "s", source: "during", text: `Fact ${written} written during the switch.` });
      written++;
    }
  })();
  const nonAdmin = await svc.call("writer", "POST", "/v1/models", { embedding: TO });
  console.log(`${nonAdmin.status === 403 ? "pass" : "FAIL"}  a non-admin switch is refused before any model loads (${nonAdmin.status})`);
  ok &&= nonAdmin.status === 403;
  const switched = (await svc.must("admin", "POST", "/v1/models", { embedding: TO })) as { spaces: Array<{ active: boolean; vectors: number; model: string }> };
  switching = false;
  await writer;
  // One more write after the switch lands in the new index directly.
  await svc.must("writer", "POST", "/v1/memories", { scope: "s", source: "after", text: "Fact written after the switch." });
  written++;
  const view = (await svc.must("admin", "GET", "/v1/models")) as typeof switched;
  const active = view.spaces.find((s) => s.active)!;
  const old = view.spaces.find((s) => !s.active)!;
  console.log(`${active.vectors === written ? "pass" : "FAIL"}  new index holds every memory (${active.vectors} of ${written}; ${written - 100} written during or after the switch)`);
  console.log(`${old && old.vectors >= 100 ? "pass" : "FAIL"}  old index kept (${old?.vectors} vectors, ${old?.model})`);
  ok &&= active.vectors === written && old !== undefined && old.vectors >= 100;
} finally {
  await svc.stop();
  await admin.query(`drop database ${name} with (force)`);
  await admin.end();
}
console.log(ok ? "switch race: all checks passed" : "switch race: FAILED");
process.exit(ok ? 0 : 1);

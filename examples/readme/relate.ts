import assert from "node:assert/strict";
import type { Relation, WriteResult } from "../../src/index";
import { demo } from "./demo";

await demo(async ({ call, scope }) => {
  const save = (text: string) => call<WriteResult>("/v1/memories", { scope, source: "operations.md", text });
  const evidence = await save("The migration lock timed out after 30 seconds.");
  const finding = await save("Raise the migration lock timeout to 120 seconds.");
  await call("/v1/relations", { from: finding.id, to: evidence.id, type: "SUPPORTED_BY" });
  const edges = await call<Relation[]>(`/v1/memories/${encodeURIComponent(finding.id)}/relations`);
  assert(edges.some((edge) => edge.to === evidence.id && edge.type === "SUPPORTED_BY"));
});

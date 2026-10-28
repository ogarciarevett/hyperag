import assert from "node:assert/strict";
import { loadPlugin } from "hyperag/plugin";
import { demo } from "./demo";

await demo(async ({ url, tokenFile, scope }) => {
  const memory = await loadPlugin({ url, tokenFile });
  await memory.health();
  const saved = await memory.save({ scope, source: "host-app", text: "Failed jobs retry three times." });
  const context = await memory.search("Failed jobs retry", { maxTokens: 400 });
  assert.equal(context.citations[0]?.memoryId, saved.id);
});

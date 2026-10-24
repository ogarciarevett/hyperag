// Scores eval/results/rerank-parity/pairs.json with the TypeScript re-ranker.
//   bun eval/rerank-parity.ts q8|fp32 > eval/results/rerank-parity/ts-<dtype>.json
// The Rust side: HYPERAG_PAIRS=<pairs> HYPERAG_SPEC=<spec> cargo test --release reranker_parity -- --ignored --nocapture
import { readFileSync } from "node:fs";
import { createLocalReranker } from "../src/adapters/embedders";

const dtype = (process.argv[2] ?? "q8") as "q8" | "fp32";
const pairs: [string, string][] = JSON.parse(
  readFileSync("eval/results/rerank-parity/pairs.json", "utf8"),
);
const r = createLocalReranker({ dtype });
const scores: number[] = [];
for (const [q, d] of pairs) scores.push((await r.rerank(q, [d]))[0]!);
console.log(JSON.stringify(scores));

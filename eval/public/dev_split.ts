// The frozen dev split for tuning on LongMemEval-S, written once before any tuning.
//   bun eval/public/dev_split.ts [--data <dir>]  ->  eval/public/splits/lme-s-dev.json
// Test: the seeded n=100 subset that memory.ts reports (`lme-s100`, seed 42). Dev: the other 400
// questions, minus every question whose evidence sessions (answer_session_ids) include a session
// that is evidence for a test question. Tuning reads dev only; the full LME-S 500 row is not held out.
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";

const { values: a } = parseArgs({
  options: {
    data: {
      type: "string",
      default: "data/memory",
    },
  },
});
const SEED = "42";
const raw = readFileSync(`${a.data}/longmemeval_s_cleaned.json`);
type Q = { question_id: string; answer_session_ids: string[] };
const data = JSON.parse(raw.toString("utf8")) as Q[];
// The same order as memory.ts's `seeded`: ascending sha256 of seed + question id.
const key = (id: string) =>
  createHash("sha256")
    .update(SEED + id)
    .digest("hex");
const ordered = [...data].sort((p, q) =>
  key(p.question_id) < key(q.question_id) ? -1 : 1,
);
const test = ordered.slice(0, 100);
const testEvidence = new Set(test.flatMap((q) => q.answer_session_ids));
const rest = ordered.slice(100);
const overlapping = rest.filter((q) =>
  q.answer_session_ids.some((s) => testEvidence.has(s)),
);
const dev = rest.filter((q) => !overlapping.includes(q));
if (
  new Set([...test, ...dev].map((q) => q.question_id)).size !==
  test.length + dev.length
)
  throw new Error("dev and test share a question");

const out = {
  source: {
    file: "longmemeval_s_cleaned.json",
    sha256: createHash("sha256").update(raw).digest("hex"),
    questions: data.length,
  },
  seed: SEED,
  rule: "test = first 100 by sha256(seed + question_id); dev = the rest minus questions whose answer_session_ids overlap the test questions' answer_session_ids",
  test: test.map((q) => q.question_id),
  excludedForOverlap: overlapping.map((q) => q.question_id),
  dev: dev.map((q) => q.question_id).sort(),
};
writeFileSync(
  "eval/public/splits/lme-s-dev.json",
  JSON.stringify(out, null, 1) + "\n",
);
console.log(
  `test ${out.test.length}, dev ${out.dev.length}, excluded for evidence overlap ${out.excludedForOverlap.length}`,
);

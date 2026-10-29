import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { extractRankable } from "./rust-sql";

const sql = "m.deleted_at IS NULL\n  AND m.scope = ANY($1::text[])";
for (const declaration of [
  `pub(super) const RANKABLE: &str = "${sql}";`,
  `pub(super) const RANKABLE: &str =\n    "${sql}";`,
  `const RANKABLE : & str\n =\n "${sql}" ;`,
]) test("Rust SQL extraction preserves literal bytes across formatter layouts", () => {
  expect(extractRankable(declaration)).toBe(sql);
});

test("the shipped Rust eligibility declaration is readable without a database", () => {
  const source = readFileSync(new URL("../crates/hyperag/src/store.rs", import.meta.url), "utf8");
  expect(extractRankable(source)).toContain("m.deleted_at IS NULL");
});

test("a missing eligibility constant fails with an explicit diagnostic", () => {
  expect(() => extractRankable("const OTHER: &str = \"fixture\";")).toThrow("RANKABLE");
});

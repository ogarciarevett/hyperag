# Coding standards

Judgement rules for reviewers. Implementers do not need to read this file; reviewers apply it to every diff. Mechanical checks (typecheck, build, tests, `bun run lint`) run in CI.

## 1. Reuse first

Before writing new code, look in the shared reusable core (`src/core.ts`, `src/context.ts`, `src/fusion.ts` and `src/adapters/` for TypeScript, `crates/hyperag/src/` for Rust; new retrieval, scope or grant logic goes through them) and reuse or extend it. Duplicating logic that already lives there is a review finding.

## 2. Branching (TypeScript)

No nested (double) ternaries. When branching on one value, use a `switch` or a lookup map, not a chain of `else if`.

```ts
// Reject
const tone = n >= 80 ? "urgent" : n >= 60 ? "warning" : "normal";
// Accept
const tone = pickTone(n); // switch (true) or a small ordered table, one place
```

Mechanical part: Biome `style/noNestedTernary` is enabled in `biome.json` and runs as `bun run lint` (CI). No linter rule covers long `else if` chains, so reviewers check that by eye.

## 3. Validate first, at the boundary

Parse the whole input against a schema once, at the entry point, and fail with one typed validation error. Reject a function that checks fields line by line and returns or throws a different ad hoc error for each.

## 4. Schema libraries

TypeScript uses Zod wherever performance allows, and always for endpoints, MCP tools, config and messages. Hot loops may validate explicitly at their boundary instead; say why in the PR. The HTTP server, MCP tools, plugin entry points and config are boundaries; retrieval inner loops are the performance exception.

Rust uses `serde` plus a validation crate (`garde` or `validator`) or typed newtypes whose constructors validate. Parse into the validated type at the boundary and pass only that type inward. Do not add a crate just to satisfy this rule: a newtype with a checked constructor is enough for a small shape. The Rust crate follows the same rule for its HTTP and MCP inputs.

## 5. Glossary

`GLOSSARY.md` is required and kept current. Every domain term with a collision risk is defined there. In this codebase "contract" means a DTO or schema (a request or response shape), not a smart contract. Reviewers flag any undefined term a diff introduces.

## 6. Unit tests ship with the code

Every added piece of code ships with unit tests, using fixtures or mocks where needed. TypeScript tests live in separate files under `test/`, mirroring the source file name. Rust tests in `crates/hyperag/` live in the same file as the code, in `#[cfg(test)] mod tests`, or in the existing `*_tests.rs` modules where a module already uses that layout.

A diff that adds logic without a unit test is a review FAIL.

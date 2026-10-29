# Contributing

1. Start the stack: `docker compose -p hyperag up -d` (Postgres with pgvector, Neo4j on loopback).
2. Rust: `cargo build --release && cargo test --release`. TypeScript reference: `bun install && bun test --timeout 180000`.
3. Behaviour checks against a running service: `HYPERAG_SERVICE_CMD="target/release/hyperag serve" bun eval/scenarios.ts --hyperag http --build rust --systems hyperag`, `bun eval/mcp-smoke.ts`, `bun eval/switch-race.ts`.
4. A change that touches retrieval re-runs the evaluation in `eval/` and reports every metric, not only the ones that improved. Never tune on a held-out set.
5. One change per pull request, with the commands you ran and their output.

## AI-assisted contributions

AI assistance is welcome. In the PR description, disclose each tool, the model identifier when exposed, and the scope of its help. If the model is not exposed, say `unknown (not exposed)`; do not guess. The last line of the pull request template is where this goes.

Example disclosure (replace the example with accurate details):

```text
AI assistance: tool = coding assistant; model = unknown (not exposed).
Scope: helped draft the change and tests.
Human responsibility: I reviewed and understand every submitted change.
Validation: list actual checks and results, or explain checks not run.
```

A named human must review and understand the complete change, verify its tests and claims, check security and privacy, and confirm the right to contribute all code and dependencies under the applicable licenses. AI assistance does not waive existing review, signing, coverage, or CI requirements. Do not include secrets, personal data, private prompts, conversation transcripts, or hidden reasoning in the disclosure.

Before pushing a Rust change, run `cargo fmt --all -- --check`, `cargo clippy --release -p hyperag --lib --no-deps`, and focused database-free unit regressions for the changed behavior. CI repeats formatting and lint checks before its full build and test suite. Keep any baseline formatting repair in a separate mechanical commit and report its scope.

Before publishing or editing a PR body, validate the draft against the current head with the verifier in the [pinned shared evidence workflow](https://github.com/ogarciarevett/.github/blob/b6e59548f3e18d63d5b953fb4fd3fa86b0782b55/.github/workflows/evidence.yml). Require a nonempty `## Evidence (head XXXXXXX)` section, where `XXXXXXX` is the first seven characters of the current full commit SHA; plain `## Evidence` is insufficient. Revalidate after every head or body change. After publication, run the existing shared `scripts/check-pr.cjs OWNER/REPO NUMBER` validator and confirm the current-head hosted evidence check passes.

Before pushing changes to the runnable README fixture or its database cleanup, run `bun test test/readme-pool.test.ts` and `bun examples/readme/run.ts` against the isolated local stack. Require both the example assertions and process exit status to pass; force-dropping a fixture database before all client disconnect events complete can fail after assertions print PASS.

New commits must use the owner's GitHub noreply identity in repository-local Git configuration. Before pushing, audit both author and committer metadata and any message trailers for the newly authored commit range; do not rewrite historical feature commits as a metadata cleanup shortcut.

# Contributing

1. Start the stack: `docker compose -p hyperag up -d` (Postgres with pgvector, Neo4j on loopback).
2. Rust: `cargo build --release && cargo test --release`. TypeScript reference: `bun install && bun test --timeout 180000`.
3. Behaviour checks against a running service: `HYPERAG_SERVICE_CMD="target/release/hyperag serve" bun eval/scenarios.ts --hyperag http --build rust --systems hyperag`, `bun eval/mcp-smoke.ts`, `bun eval/switch-race.ts`.
4. A change that touches retrieval re-runs the evaluation in `eval/` and reports every metric, not only the ones that improved. Never tune on a held-out set.
5. One change per pull request, with the commands you ran and their output.

## AI-assisted contributions

AI assistance is welcome. In the PR description, disclose each tool, the model identifier when exposed, and the scope of its help. If the model is not exposed, say `unknown (not exposed)`; do not guess. The last line of the pull request template is where this goes. This disclosure requirement applies to external contributors; the owner and authorized agents leave that template line blank.

Example disclosure (replace the example with accurate details):

```text
AI assistance: tool = coding assistant; model = unknown (not exposed).
Scope: helped draft the change and tests.
Human responsibility: I reviewed and understand every submitted change.
Validation: list actual checks and results, or explain checks not run.
```

A named human must review and understand the complete change, verify its tests and claims, check security and privacy, and confirm the right to contribute all code and dependencies under the applicable licenses. AI assistance does not waive existing review, signing, coverage, or CI requirements. Do not include secrets, personal data, private prompts, conversation transcripts, or hidden reasoning in the disclosure.

## Local validation and reviewer evidence

Before pushing a Rust change, run `cargo fmt --all -- --check`, `cargo clippy --release -p hyperag --lib --no-deps`, and focused database-free unit regressions for the changed behavior. CI repeats formatting and lint checks before its full build and test suite. Keep any baseline formatting repair in a separate mechanical commit and report its scope.

Before publishing or editing a PR body, bind truthful reviewer evidence to the current full commit SHA. Report actual commands, results, relevant limitations and rollback risks; update that evidence after head or body changes. Reviewers assess coverage and validity, and source or fixture proof does not establish live-service acceptance.

Before pushing changes to the runnable README fixture or its database cleanup, run `bun test test/readme-pool.test.ts` and `bun examples/readme/run.ts` against the isolated local stack. Require both the example assertions and process exit status to pass; force-dropping a fixture database before all client disconnect events complete can fail after assertions print PASS.

New commits must use the owner's GitHub noreply identity in repository-local Git configuration. Before pushing, audit both author and committer metadata and any message trailers for the newly authored commit range; do not rewrite historical feature commits as a metadata cleanup shortcut.

Before pushing source or formatting changes, run `bun run test:db-free` as well as the relevant language checks. This service-free TypeScript suite includes temporary SQLite fixtures and loopback HTTP mocks; maintain its file list when adding tests. SQL fixture extraction must tolerate Rust formatter whitespace while preserving SQL literal bytes.

Database fixtures that execute the shipped Rust SQL must install its required production index migrations before asserting plans or performance. When that migration list or SQL changes, run the affected database-backed fixture locally; service-free tests alone do not prove query-plan parity.

Query-plan fixtures must separate semantic safety from planner choice: small or dense cache scans can be legitimate, while source-table scans remain forbidden for ready indexed retrieval. Prove narrow indexed/bounded work on a realistically sized, diverse-vocabulary fixture and preserve exact score/order parity and the existing execution budgets.

## Shared language quality gate

Use the existing Rust 1.99.0, Bun 1.4.3 and Node 24.22.1 runtimes. Set up
contributor-only Python tools with Python 3.12 or newer:

```sh
python3 -m venv .quality-venv
.quality-venv/bin/python -m pip install --require-hashes -r requirements-quality.txt
bun install --frozen-lockfile --ignore-scripts
.quality-venv/bin/python scripts/quality-gate.py
.quality-venv/bin/python -I scripts/test_quality_gate.py
```

The read-only command checks Black 26.10.1, Ruff 0.14.3, Biome 2.6.4,
strict TypeScript types, and the existing Rust fmt/clippy gates. The first
failure stops delivery; it never formats files or downloads models. CI and
release builds depend on the same command. The `python`, `typescript`, and
`rust` arguments select an individual language for diagnosis. The `all` and
`typescript` selections first require `package.json` `engines.node` to stay
`^24.22.1`, a stable Node `>=24.22.1 <25`, and exactly Bun 1.4.3; the Python and
Rust selections do not inspect those runtimes.

Install `.githooks/pre-push` only after inspecting existing local hooks. For an
isolated worktree, enable Git's `extensions.worktreeConfig` in repository-local
config and set `core.hooksPath` to that worktree's absolute `.githooks` path with
`git config --worktree`; preserve any existing hook rather than replacing it.
No global Git configuration is needed.

Related or dependent feature PRs must form an official `gh stack`. Record the
exact stack/base/head relationship and current-head validation for every layer.
Reviewers return CHANGES for unlinked dependent feature PRs. Merge an accepted
stack through `gh stack merge` only with the existing named-human and release
authority; a linked stack or source PASS does not grant merge or runtime adoption.

Ruff uses E4/E7/E9/F. E402 has only 18 inline applicability annotations:
15 imports after the native guard's early budget clock and three imports after
the read-only pilot path bootstrap. Preserve those ordering contracts; no global
ignore applies. The unannotated E402 negative fixture must still block delivery.

# Contributing

1. Start the stack: `docker compose -p hyperag up -d` (Postgres with pgvector, Neo4j on loopback).
2. Rust: `cargo build --release && cargo test --release`. TypeScript reference: `bun install && bun test --timeout 180000`.
3. Behaviour checks against a running service: `HYPERAG_SERVICE_CMD="target/release/hyperag serve" bun eval/scenarios.ts --hyperag http --build rust --systems hyperag`, `bun eval/mcp-smoke.ts`, `bun eval/switch-race.ts`.
4. A change that touches retrieval re-runs the evaluation in `eval/` and reports every metric, not only the ones that improved. Never tune on a held-out set.
5. One change per pull request, with the commands you ran and their output.

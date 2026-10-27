# Native uncached hooks

`hyperag hook session-start` reads its credential, configuration and common Git repository metadata directly. The native hook uses a current-thread runtime; service commands retain their existing runtime. No Python or Git wrapper is needed for this path.

An optional absolute `token_file` in `hooks.toml` points to a JSON file with a `token` field. It takes precedence over `token_env`. The file must be owned by the current user with mode 0600; symlinks and unsafe ancestry fail closed before HTTP requests. Opened-file identity checks are retained in `hook_credentials.rs`.

Every project hook fetches fresh `GET /v1/scopes` permissions. An eligible session-start then fetches `GET /v1/recent`; an ungranted project skips quietly. There is no grant cache, generation counter, TTL, cache lock or invalidation path, and reads create no local runtime state. Permission revocation and regrant are reflected on the next call. Session-start reads remain synchronous; prompt and session-end calls likewise wait for their writes.

## Hosted measurement behind the decision

Before removing the cache, an isolated hosted measurement applied an uncached native-path patch to the measurement checkout only. Each cohort had 50 measured calls and one excluded warmup. Every invocation made exactly one scopes GET and one recent GET, with zero POSTs and no local hook state.

| Cohort | p50 ms | p95 ms | Errors |
| --- | ---: | ---: | ---: |
| Fast scopes fixture | 6.548 | 7.310 | 0 |
| Scopes GET delayed by 100 ms | 107.279 | 107.812 | 0 |

These are full native-process wall times against synthetic loopback HTTP fixtures, not live-service or organic-session acceptance. The measured margin supports fresh permission requests without the cache's concurrency machinery. The sanitized run reference and results are retained in `docs/evidence/native-hook-no-cache-2026-10-27.json`.

Hosted CI runs `HYPERAG_BIN=target/release/hyperag python3 crates/hyperag/tests/hooks_fast_path.py`. Its actual-binary fixtures report separate 50-call uncached quantiles, exact request counts and no state creation; exercise immediate allow→deny→regrant, credential/endpoint/project isolation, synchronous reads and writes, owner-only credential protections and error handling. No local service, model, database or live credential is used.

This is stack A. Stack B adds opt-in detached writes and failure accounting on top. Live hooks remain OFF; deployment and guarded acceptance still require coordinator GO.

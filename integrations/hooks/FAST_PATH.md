# Native hook read fast path

`hyperag hook session-start` reads its credential, configuration and common Git repository metadata directly. The native hook uses a current-thread runtime; service commands retain their existing runtime. No Python or Git wrapper is needed for this path.

An optional absolute `token_file` in `hooks.toml` points to a JSON file with a `token` field. It takes precedence over `token_env`. The file must be owned by the current user with mode 0600; symlinks and unsafe ancestry fail closed before HTTP requests.

Session-start caches each project's grant eligibility for at most five seconds from the start of the permission request. A private monotonic generation orders cross-process requests; an older response cannot replace a newer decision or receive a fresh TTL when it arrives late. Superseded callers use a valid cached decision or quietly skip. Cache entries are bound to the endpoint and credential, limited to 64 current entries, and contain only scope, eligibility, expiry and generation. Private directories under `~/.hyperag-hooks/` are mode 0700; files are mode 0600. Concurrent cache admission never waits for another hook; contention skips caching. The server still authorizes every recent-context request. HTTP 401/403/404 invalidation takes the publication lock and advances the generation before removing the affected read cache. Delayed in-flight grant replies therefore cannot recreate an invalidated allow.

A project without the required grants skips quietly. Session-start reads stay synchronous. Prompt and session-end calls always fetch current grants and wait for their writes; this change introduces no detached worker, queue, status command or activation.

Hosted CI runs `HYPERAG_BIN=target/release/hyperag python3 crates/hyperag/tests/hooks_fast_path.py`. The actual binary fixture reports 20-call native session-start p50/p95/errors, verifies scope-request reuse and actual five-second expiry, credential/endpoint/project isolation, synchronous reads and writes, private credential protections, delayed out-of-order grant decisions and request-start expiry. Fixture timing is separate from live guarded acceptance. No local service, model, database or live credential is used.

This is stack A. Stack B adds opt-in detached writes and their failure accounting on top. Live hooks remain OFF; a later deployment and guarded acceptance watch require coordinator GO.

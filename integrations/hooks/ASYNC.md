# Opt-in asynchronous hook writes

Prompt and session-end hooks can return after a detached worker accepts their write. Session-start reads remain synchronous. The default is still synchronous writes.

Configure the native hook in the existing `hooks.toml`:

```toml
async_writes = true
token_file = "/absolute/private/hooks-credential.json"
async_active_marker = "/absolute/private/hooks-active"
```

The credential file contains a JSON `token` field, is owned by the current user, and has mode 0600. Its ancestry must be trusted. Async mode requires this file; its credential takes precedence over inherited token variables. Use a dedicated credential with the existing read grants and only the permitted project write grants. This configuration never creates or widens grants. The optional active marker must have an absolute path; its removal cancels pending writes after any request already in flight finishes.

Use the central installer's native `hyperag hook` commands. The native path reads credentials and common Git repository metadata directly, without per-call Python or Git subprocesses. Hook commands use a current-thread runtime; the service keeps its existing runtime.

## Fresh project permission checks

Every native call for a derived project fetches `/v1/scopes` with that credential before reading or enqueuing. No grant decisions, expiry timestamps, generations or invalidation files are cached. A missing read or write grant produces a category-only `skipped_ungranted` record: no memory write, stderr or error trip. A grant revocation or regrant is observed on the next call without waiting for a TTL. The server independently authorizes every read and background write; revocation after an enqueue can therefore still reject its delivery, which is recorded as a background failure. Hooks never create or widen grants.

## Queue and delivery

The worker accepts at most 64 queued/in-flight events and 1 MiB of serialized payload bytes. These are payload limits, not a total process RSS guarantee. The IPC reader reserves capacity before allocating a body, then durably records acceptance before publishing the item to the HTTP writer. A credit response reserves capacity; only the final acknowledgement confirms acceptance. Native enqueue has a 150 ms deadline.

One worker per user, endpoint and credential performs sequential HTTP writes, each with a two-second deadline. It runs in a separate process group with no inherited credentials beyond its explicit named settings and no standard streams. It exits after 30 seconds idle, and has a ten-second final drain deadline. Removing the active marker cancels queued writes. Accepted payloads stay in memory; they are not persisted in a spool. Tokens, prompts and memory text are absent from queue metadata and command arguments.

An acknowledgement means **queued**, not **saved**. A crash may occur after the server commits but before local accounting records success. Such delivery is unknown. The worker does not automatically retry or replay an uncertain write, avoiding duplicate memories.

## Failure records and inspection

Private metadata lives under `~/.hyperag-hooks/`, keyed by endpoint and credential. Directories are mode 0700; regular files and the socket are mode 0600. Metadata contains bounded counters, timestamps, process identity and category names. `status.json` tracks accepted, stored, failed, cancelled and rejected events; `enqueue-errors.json` separately tracks foreground rejection and quiet-skip categories so concurrent hook processes cannot overwrite worker accounting.

Queue-full, invalid IPC, enqueue timeout, acknowledgement failure, startup failure, HTTP authorization failure, backend/transport failure, cancellation, writer failure, drain deadline and abandoned queue cases are categorized. A background delivery failure blocks new writes and cancels remaining queued items with counted categories. Foreground enqueue failures also produce stderr for a supervising guard.

Run `hyperag hook-writer-status` with the same protected hook configuration to inspect counters. It checks exclusive worker ownership, rather than trusting a PID alone. If no worker owns the queue and accepted deliveries remain unresolved, it records `lost_queue`, marks those deliveries unknown/failed and blocks replacement. Inspecting status must not replay writes or silently reset that latch. Resolve the delivery uncertainty before explicitly replacing a blocked queue's state.

## Deployment boundary

This feature does not install the live latency/error guard or activate hooks. Before activation, an external watchdog must inspect native foreground errors and worker status even when no later hook arrives. Any worker failure, rejection, timeout, acknowledgement error or newly detected abandoned queue must disable hooks and dispatch the detached verified restore path. Quiet ungranted-project skips are excluded from error trips.

The approved live watch remains one unscored warm-up, immediate trip on any error/stderr/timeout, hard trip on any single call over 1000 ms, and all-call p95 above 300 ms once at least 20 measured calls exist. Report p50/p95/errors at 50 controlled and 100 total calls. The watchdog integration and a fresh activation require coordinator GO; native fixture timings do not establish those live acceptance results.

Hosted CI runs `HYPERAG_BIN=target/release/hyperag python3 crates/hyperag/tests/hooks_async.py` against synthetic HTTP fixtures. Tests exercise the actual native entrypoints, slow-write detachment and order, synchronous reads, fresh-per-call permissions, immediate denial/regrant and revocation after enqueue, no-grant skips, queue limits, process-group survival, abandoned delivery accounting, credential protections and native latency quantiles. No shared service port, real credential, model or database is used by this suite.

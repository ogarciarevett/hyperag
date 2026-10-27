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

Every native call for a configured, overridden or derived scope fetches `/v1/scopes` with that credential before reading or enqueuing. No grant decisions, expiry timestamps, generations or invalidation files are cached. A missing read or write grant produces a category-only `skipped_ungranted` record: no memory write, stderr or error trip. A grant revocation or regrant is observed on the next call without waiting for a TTL. The server independently authorizes every read and background write; revocation after an enqueue can therefore still reject its delivery, which is recorded as a background failure. Hooks never create or widen grants.

## Queue and delivery

The worker accepts at most 64 queued/in-flight events and 1 MiB of serialized payload bytes. These are payload limits, not a total process RSS guarantee. The IPC reader reserves capacity before allocating a body, then durably records acceptance before publishing the item to the HTTP writer. A credit response reserves capacity; only the final acknowledgement confirms acceptance. Native enqueue has a 150 ms deadline.

One worker per user, endpoint and credential performs sequential HTTP writes, each with a two-second deadline. It runs in a separate process group with no inherited credentials beyond its explicit named settings and no standard streams. It exits after 30 seconds idle, and has a ten-second final drain deadline. Removing the active marker cancels queued writes. Accepted payloads stay in memory; they are not persisted in a spool. Tokens, prompts and memory text are absent from queue metadata and command arguments.

An acknowledgement means **queued**, not **saved**. A crash may occur after the server commits but before local accounting records success. Such delivery is unknown. The worker does not automatically retry or replay an uncertain write, avoiding duplicate memories.

## Failure records and inspection

Private metadata lives under `~/.hyperag-hooks/`, keyed by endpoint and credential. Directories are mode 0700; regular files and the socket are mode 0600. Metadata contains bounded counters, timestamps, process identity and category names. `status.json` tracks accepted, stored, failed, cancelled and rejected events; `enqueue-errors.json` separately tracks foreground rejection and quiet-skip categories so concurrent hook processes cannot overwrite worker accounting.

Queue-full, invalid IPC, enqueue timeout, acknowledgement failure, startup failure, HTTP authorization failure, backend/transport failure, cancellation, writer failure, drain deadline and abandoned queue cases are categorized. A background delivery failure blocks new writes and cancels remaining queued items with counted categories. Foreground enqueue failures also produce stderr for a supervising guard.

Run `hyperag hook-writer-status` with the same protected hook configuration to inspect counters. It checks exclusive worker ownership, rather than trusting a PID alone. If no worker owns the queue and accepted deliveries remain unresolved, it records `lost_queue`, marks those deliveries unknown/failed and blocks replacement. Inspecting status must not replay writes or silently reset that latch. Resolve the delivery uncertainty before explicitly replacing a blocked queue's state.

## Deployment boundary

The native writer can be supervised by `integrations/hooks/native_guard.py`. It does not activate hooks or change service builds. Use a private JSON guard configuration with absolute paths:

```json
{
  "binary": "/absolute/hyperag",
  "active_marker": "/absolute/private/hooks-active",
  "state_dir": "/absolute/private/watch-unique-to-activation",
  "url": "http://127.0.0.1:7481",
  "native_env": {"HOME": "/absolute/home", "XDG_CONFIG_HOME": "/absolute/home/.config"},
  "rollback_command": ["/absolute/verified-rollback-command"]
}
```

The guard never reads a token or fetches grants. Its native child uses `token_file` and performs the existing fresh scopes check. No grant cache is introduced. Pre-arm the watchdog with `python3 integrations/hooks/native_guard.py --config /absolute/guard.json arm` before controlled calls; install a thin launcher that delegates its native `hook EVENT --harness NAME` arguments to the guard's `hook` command. Central installer ownership markers must be preserved so rollback removes only managed entries. The launcher's stdin/stdout remain the harness channels. A private inherited pipe carries only category and scope names; tokens, input, memory context and raw stderr are never recorded.

The watchdog runs in a detached session with a single-flight lock and readiness handshake. It polls native status independently of foreground calls, validates async mode, a dedicated token file and the matching active marker, and trips on failed/rejected/blocked delivery or inspection failure. Inspection also classifies an abandoned accepted queue as `lost_queue`. Trip handling removes the marker before dispatching the existing detached verified recovery command. No subsequent hook is required to notice a delivery failure. Missing grants remain category-only quiet skips, with no write, stderr or error trip.

The live policy is one unscored warm-up, immediate trip on any error/stderr/timeout (including warm-up), hard trip on any single call over 1000 ms, and p95 above 300 ms across all measured calls once at least 20 exist. Reports at 50 and 100 calls contain p50/p95/error counts. A thin trusted launcher may set the named `HYPERAG_HOOK_STARTED_AT` epoch timestamp before starting Python so guard timing includes interpreter startup. Guard elapsed time ends inside the serialized monitor section before final metadata persistence and process exit; hosted latency cohorts measure the full subprocess wall time, and activation must retain its external controlled-call timing check. An in-flight deadline also detects hung finalization. Use a fresh state directory for each activation; the guard does not silently reset failure accounting. Activation still requires the merged integration, coordinator GO and the chief's machine clearance. Hosted fixtures do not establish live 50+50 acceptance.

Hosted CI runs `HYPERAG_BIN=target/release/hyperag python3 crates/hyperag/tests/hooks_async.py` against synthetic HTTP fixtures. Tests exercise the actual native entrypoints, slow-write detachment and order, synchronous reads, fresh-per-call permissions, immediate denial/regrant and revocation after enqueue, no-grant skips, queue limits, process-group survival, abandoned delivery accounting, credential protections and native latency quantiles. No shared service port, real credential, model or database is used by this suite.

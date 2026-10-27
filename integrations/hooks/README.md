# Recovery from a harness hook

Harness commands can be killed after three seconds. A guard must disable hooks
first, then dispatch service recovery outside that process group:

```python
from recovery import dispatch, status_for

active_marker.unlink(missing_ok=True)
worker_pid = dispatch(["/absolute/path/to/rollback.sh"], private_state_directory)
status = status_for(private_state_directory, worker_pid)
```

`dispatch(command, state_dir)` returns the detached worker PID promptly. Concurrent
workers share a nonblocking lock, so only one recovery command runs. Always use
`status_for(state_dir, worker_pid)` for `running`, `succeeded`, or `failed`: it
returns `None` until that PID owns the status, preventing a previous attempt's
success from being reused. A suppressed duplicate never owns a status; only the
winning worker PID does. Launch success does not establish restoration. The
rollback command must check service health before exiting zero.

Use a trusted private state directory and absolute command paths. Recovery inherits
only a minimal `PATH`; the rollback command loads its required named service
settings from protected files. Never put credentials or prompt text in command
arguments. All standard streams are discarded. Status files are atomic and mode
0600, containing timestamps, worker PID, and an exit code or exception type.

Run the isolated regression suite with
`python3 -I integrations/hooks/test_recovery.py`. It kills a simulated hook process
group midway through recovery and verifies restoration still completes.

Run the production guard entrypoint with a private configuration file:

```sh
python3 integrations/hooks/guard.py --config /private/guard.json hook prompt --harness claude-code
```

The configuration supplies `binary`, `credential_file`, `active_marker`,
`state_dir`, `url`, and `rollback_command` (an absolute executable and arguments).
The mode-0600 credential file contains the token and its explicit `read_scopes`
and `write_scopes`; it is never put in arguments or logs. `token_env` defaults to
`HYPERAG_TOKEN` and may instead be `HYPERAG_HOOKS_TOKEN` to match hooks.toml.
An absent active marker makes cached commands no-ops. The real entrypoint checks
canonical project eligibility before invoking the hook. A missing grant writes
only `skipped_ungranted`, with no child, write, stderr, trip or recovery. Eligible
calls run with the dedicated token and record only timing/error metadata. Any
actual error or rolling-cohort p95 above300ms removes the marker and dispatches
detached recovery. Neither configuration nor installation grants any permissions.

The Rust hook independently GETs authenticated `/v1/scopes` before recent or write
requests. It checks current caller read/write grants and silently skips an
ungranted project, including stale permissions that a local snapshot still lists.
Both Rust and TypeScript services implement this read-only caller-scoped endpoint.
`test_guard.py` drives the actual CLI through a positive write control then an
ungranted call with zero writes, empty stderr, no trip, and unchanged grants.
The binary hook contract tests and service smoke test cover the Rust entrypoint.

`rollback.py /absolute/path/to/private-config.json` is the included Linux/macOS service
restore command. Its mode-0600 config supplies `current` process identity from
`rollback.identity(pid)`, `restore` (`argv`, binary `sha256`, and explicitly named
service `env`), and a loopback `/healthz` URL. It verifies the current executable
and start time before stopping it, then checks the restored executable hash and
health before reporting success in `rollback-status.json`. It never loads a
captured process environment or inherits caller credentials. An optional bounded
`restart_delay_seconds` allows a holdoff between stopping and restoring.
Linux checks the running executable through `/proc`; macOS uses only `ps` command
path, start-time and process-state metadata plus the executable's disk hash. Other
platforms fail closed. An interrupted attempt can resume after its old PID exits,
provided the health endpoint is down; an already healthy unowned endpoint is
never stopped or replaced. `test_rollback.py` checks these branches with mocks.

Hosted Linux CI builds the pinned prior service, starts the current Tantivy writer
on an ephemeral port and an owned database with models disabled, then dispatches
this command. The regression kills the hook process group more than three seconds
after stopping the service; recovery must still restore health and the prior
binary identity. The RED variant shares the hook group and must fail. Neither
variant contacts ports 7481/7482.

On macOS the process-state, start-time and executable checks are separate `ps`
snapshots. A service that becomes defunct between them is treated as exited so
restoration continues; an invalid executable for a still-live process or a changed
start marker still fails closed. Failure records retain the restoration phase and
changed identity key names without copying exception text. Hosted service tests
force this Darwin metadata interleaving around a real Hyperag shutdown, then
require the pinned prior executable and health to return under a guard trip.

Native hook credentials and fresh uncached permission checks are documented in [FAST_PATH.md](FAST_PATH.md).

Opt-in native asynchronous writes are documented in [ASYNC.md](ASYNC.md). That path
requires separate guard/watchdog integration before any live activation.


For a forward replacement, run `python3 integrations/hooks/switch.py /absolute/private/switch.json`.
The owner-only configuration contains `current` (the verified prior identity), `restore` and
`target` launch records (`argv`, binary `sha256`, named allowlisted `env`), and a loopback
`health_url`. It writes the rollback record before stopping and after acquiring the new
identity, then verifies health. Failure restores the prior service through the same helper.
`switch-status.json` retains each phase/category and timestamps, without exception text,
arguments or environment values. Both paths verify executable ownership before signalling.
After SIGTERM they share `rollback.wait_exited`: it checks process birth and state every
50 ms, without reading executable metadata that macOS can discard during teardown.
A changed birth fails closed. If failback finds an already-invalid executable path, it
sends no signal and waits for that recorded birth to disappear before launching the prior
build; a live process that does not exit times out.

`fixtures/macos-stop-2026-10-27.json` records the one bounded macOS probe on trial port
7482 (a42ad2c): after SIGTERM, zombie at 2.779 ms, absent at 6.099 ms. It did **not**
reproduce the production invalid-path window; no second local probe was run. The fixture
contains field names/states and relative timing only. Unit tests replay it. Hosted CI also
extends teardown with a clearly synthetic 100 ms non-zombie/invalid-path window on real
services, proves legacy executable polling fails, and requires successful forward switch
and failback with binary identity and health on isolated ports. Updating these Python helpers
does not require rebuilding an existing service binary. No live activation is implied.


The native guard uses `clock_gettime(CLOCK_MONOTONIC_RAW)` for cross-process
watchdog heartbeats and in-flight call timestamps. Python 3.9 on macOS gives
`time.monotonic()` a process-relative origin; it remains suitable for deadlines
inside one process, but must not be compared with another process's timestamps.
In-flight markers record PID, shared-clock start, wall-clock start for diagnosis,
and agent/event names only. Before a timeout, the watchdog probes PID liveness
and rechecks that the marker still exists. Removed completion markers are ignored;
a disappeared PID with a remaining marker is `foreground_lost`, not a timeout.
The 1 s deadline and 30 ms latency margin are unchanged. Hosted tests run real
native calls and a detached watchdog with deliberately different process-local
monotonic origins, plus marker-removal and dead-PID regressions. No activation
or service rebuild is implied by this Python-only repair.


The native subprocess supervisor polls completion before applying its deadline.
A supervisor that resumes after the child has already exited drains the completed
result; it times out a child that is still running. Output, report and stderr
streams remain size-bounded while draining. The foreground guard still applies
its full-call 1000 ms limit and the approved all-call p95 policy with 30 ms margin.
Hosted tests force a successful status child to exit, pause its supervisor past
250 ms, and require a successful result; a separate running child must still time
out. The slow-fsync fixture now retains stage timings and category-only metadata
on failure so a generic recovery message no longer destroys the diagnosis.


Every native-guard `trip.json` now includes `stage`, `operation`, `child_pid`,
`started_at` (wall time), and measured `duration_ms`, alongside category and
recovery metadata. A filesystem/policy operation with no child records a null PID;
that is distinct from missing tracing. Hook trips also include agent/event names.
Tracing is in memory on successful calls; only the existing trip path persists it.
No input, returned memory, token, environment, arguments or raw exception text is
recorded. Retry dispatch preserves the first failure's trace. Watchdog reporting,
status CLI, foreground checks and heartbeat each have distinct operation names.
This tracing-only change leaves all budgets, retry counts and hook policy intact.


### Chief-approved background probe policy (2026-10-27)

A background health probe is not a user hook latency sample. Its native status
command has a **2 s budget**. Only **three consecutive failed probes** request
recovery; a successful probe resets the count. Every failed probe is appended to
`probe-failures.jsonl` with category, stage `background_probe`, operation,
child PID, start time, measured duration and consecutive-failure count. The third
failure's live `trip.json` retains the same operation trace. Native CLI exit,
stderr, timeout, malformed response or configuration mismatch are probe failures.
A valid status that reports a real async delivery/drop failure remains an immediate
real-write failure, not a transient probe failure.

One probe runs at a time in a background thread. The independent watchdog keeps
publishing heartbeats and supervising foreground calls during a slow probe; a
pending probe never makes a healthy fast hook fail the heartbeat freshness check.
Every foreground hook error or stderr still trips immediately. The full-hook
1000 ms hard limit, one unscored warm-up, global p95 after 20 measured calls above
300 ms, quiet skips and fixed 30 ms margin are unchanged. Probe durations are
excluded from user-hook p50/p95. Successful calls add no foreground fsync.

This reading is approved by the chief. Deployment still requires merge and a
fresh coordinator confirmation, then the same 50 controlled plus 50 organic watch.

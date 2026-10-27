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

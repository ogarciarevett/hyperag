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

`guard.scope_for(cwd)` derives a project scope from the common Git repository, so
a linked worktree named `chief` still uses its repository's scope. Apply explicit
scope overrides in the caller. Before invoking a hook, call
`guard.check_scope(scope, credential, log)` with its read/write scope lists. An
ungranted project returns false and logs only `skipped_ungranted`: skip the hook
without a write, stderr, or recovery. Non-project overrides are configuration
errors. Run `python3 -I integrations/hooks/test_guard.py` for these contracts.

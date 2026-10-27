# Recovery from a harness hook

Harness commands can be killed after three seconds. A guard must disable hooks
first, then dispatch service recovery outside that process group:

```python
from recovery import dispatch

active_marker.unlink(missing_ok=True)
dispatch(["/absolute/path/to/rollback.sh"], private_state_directory)
```

`dispatch(command, state_dir)` returns the detached worker PID promptly. Concurrent
workers share a nonblocking lock, so only one recovery command runs. Read
`state_dir/status.json` for `running`, `succeeded`, or `failed`; a successful launch
does not establish successful restoration. The rollback command must check service
health before exiting zero.

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

# Hyperag plugin for Claude Code

Adds the Hyperag MCP server and three hooks:

- **SessionStart**: the newest memories of the project's scope (`project:<git root folder>`, or `HYPERAG_SCOPE`) are added to the session's context.
- **UserPromptSubmit**: each prompt is stored in that scope (kind `prompt`).
- **SessionEnd**: a short marker that the session ended (kind `session`).

Each git worktree has its own `.git`, so each worktree folder is its own scope; set `HYPERAG_SCOPE` to share one scope across the worktrees of a repository. The session-start header line is printed outside the 12,000-byte budget.

Needs `hyperag` on `PATH`, a running service, and in the environment `HYPERAG_URL` (default `http://127.0.0.1:7480`) and `HYPERAG_TOKEN` (a token from `hyperag token <principal>` with read and write on the project scopes). A hook never fails a session: errors go to stderr and it exits 0.

Install from a local checkout: `claude plugin marketplace add <path to this repo>/integrations/claude-code` then `claude plugin install hyperag@hyperag-local`.

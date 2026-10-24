# MCP


Hyperag ships an MCP server so any MCP client (coding agents, chat clients) uses the same memory.
Every tool call goes to the Hyperag service with the agent's own token, so the service checks
identity and scopes on each call.

- stdio: `hyperag mcp --url http://127.0.0.1:7480` with the agent's token in `HYPERAG_TOKEN`.
- HTTP: the service serves MCP at `/mcp`; send `Authorization: Bearer <token>`.

Tools: `ingest`, `search` (context with numbered citations), `update`, `invalidate`, `delete`,
`relate`, `explain`, `models_list`, `models_set` (admin), `health`.

Example client config:

```json
{ "mcpServers": { "hyperag": { "command": "hyperag", "args": ["mcp", "--url", "http://127.0.0.1:7480"], "env": { "HYPERAG_TOKEN": "<token>" } } } }
```

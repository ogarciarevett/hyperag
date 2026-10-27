# Host plugin contract

`hyperag/plugin` is a small remote ESM client in the existing npm package. It calls
[the HTTP API](api.md); identity, grants and retrieval stay in the running service.
It does not open a database, load models or start background jobs. The existing
[MCP server](mcp.md) and harness hooks remain alternative host integrations.

## Install and load

This change prepares **0.2.1**, which has not been published to a registry. After
release, pin `hyperag` to `0.2.1`. Before release, run `bun install --frozen-lockfile`
and `npm pack` in a reviewed checkout, then install its `hyperag-0.2.1.tgz` in the
host with `npm install --save-exact /path/to/hyperag-0.2.1.tgz`.
Use Node matching the package engines or Bun. No extra plugin dependency is needed.

```ts
import { loadPlugin } from "hyperag/plugin";
const memory = await loadPlugin({
  url: "http://127.0.0.1:7480",
  tokenFile: "/private/host/agent.token",
});
await memory.health();
```

Use an origin URL: HTTPS for remote services; HTTP only for loopback. Userinfo,
paths, query strings, fragments and redirects are refused. The optional
`timeoutMs` defaults to 30,000 (valid range 1–300,000), including response reading.
There is no automatic retry or disposal step. Reload after rotating the token.
See [the runnable example host](../examples/plugin-host.ts).

## Scoped credentials

The service administrator creates a dedicated non-admin principal using the
existing `hyperag token <principal> --file <server-registry>` command, and grants
that principal `read` and `write` only on the required project scope through
`/v1/grants`. Scope grants belong to the service, not to client configuration.
Recall searches all scopes granted to this principal; use a separate principal
when the host must be restricted to one project.

Provision the returned raw bearer token into the host file through a private
channel. Keep it in a trusted owner-controlled directory, owned by the host user
with mode `0600` or `0400`. It contains only the token and an optional final newline,
at most 4 KiB. The loader refuses token-file symlinks, non-files, other owners and group/other
permissions. POSIX ownership checks are required; unsupported platforms fail closed.
This host file differs from the server registry JSON, which stores token hashes.
Never put credentials in command arguments, source, logs or shared configuration.

## Calls and errors

| Plugin call | Existing endpoint | Result |
| --- | --- | --- |
| `save(NewMemory)` | `POST /v1/memories` | `{ id, version, created }` |
| `recall(query, RetrieveOptions?)` | `POST /v1/retrieve` | Ranked `items`, evidence flag and retrieval limitations |
| `search(query, { maxTokens?, ...RetrieveOptions }?)` | `POST /v1/context` | Context text and numbered citations |
| `health()` | `GET /v1/whoami` | `{ principal }`, proving service connectivity and valid identity |

Payloads and declarations reuse the library types. Health does not prove access
to any scope. Hosts must honor `insufficientEvidence` and cite returned sources.
Use the existing memory id/idempotency key fields for deliberate write retries.

All failures are `PluginError` with a safe `code` and numeric `status` (0 without
an HTTP status). Codes: `invalid_url`, `invalid_timeout`, `invalid_token_file`,
`request_failed` (transport, timeout, redirect or malformed JSON), and `http_error`.
For `http_error`, inspect status: 400 invalid input, 401 unauthenticated,
403 forbidden, 404 not found/hidden, 409 conflict, 429 throttled, 5xx service failure.
Server error bodies are discarded to avoid echoing private content. Successful
responses retain the existing API shapes; this client adds no wire protocol.
The npm package follows semantic versioning; `/v1` remains the service API version.

The plugin logs nothing. Memory text, queries, tokens and response bodies must not
enter host logs or telemetry; log only safe error codes/status and aggregate counts.
The reference handler reports internal failures without raw exceptions. Service
operators must apply the same privacy rule to proxies and external logging.

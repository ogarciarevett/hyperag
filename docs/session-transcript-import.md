# Import local session records and user prompts

This supplements the observation import with redacted user prompts and session metadata.
It reads JSONL source files and a read-only Engram snapshot, and writes only to the fixed
loopback memory endpoint. It never changes the live source store, transcripts or profiles.

After review and merge, run under the machine gate and shared heavy-job lock:

```sh
bun scripts/session-transcript-import.ts --db /path/to/snapshot.db --apply \
  --source codex:/path/to/sessions --source claude:/path/to/projects
```

Pass `HYPERAG_TOKEN` privately through the environment. Include every local profile's
projects root as another `--source claude:...` argument. Only JSONL files are scanned;
symlinks are not followed. Real transcript files and private import output stay local.

Sessions map to existing projects by their source session ID in the snapshot. Otherwise
the exact cwd identifies a separate scope. Prompt memories link to their session memory,
which links to observation memories with that session ID. Stable identities and checked
version updates make replay skip unchanged memories. Every written record is read back.

The importer excludes tool outputs, generated context, oversized records/prompts and
mirrored user events. Conventional credentials, private keys, phone numbers, environment
dumps and process argument lines are redacted before any prompt is stored. Inputs
containing structured env/argv dumps are replaced as a whole; arbitrary nested values
are never salvaged. Whole key names require explicit colon/equal or JSON quote-colon syntax; ordinary
prose containing session, token, key, password or auth stays unchanged. Whitespace-only
credential matching is reserved for Bearer/Basic and explicit authorization/cookie
headers. Explicit labels consume whole values, including punctuation, up to whitespace
or the closing quote. Quoted or bare phone/tel/mobile and credential key names are matched
case-insensitively; nested credential objects are removed as whole blocks. A final
pass removes 20+ character mixed letter/digit tokens (including token punctuation).
Dedicated double-dash credential flags support both space and equals forms, and URL
userinfo is removed for any scheme. National phone coverage includes Spanish nine-digit
6/7/9 numbers, UK 0-prefixed numbers, formatted North American numbers and labeled numbers. Conservative
opaque-identifier redaction may also remove non-secret identifiers. This is a bounded
local redaction policy; review new source formats before using them. No raw text is logged.
Unmatched user events survive partially mirrored streams; only corresponding mirrors are
suppressed. Missing session IDs or project/cwd mappings are counted as skips. Missing end times stay
null; the final event time is recorded separately and is not treated as session closure.
The model field is the last observed model; no missing model or timestamps are invented.

Validation: `bun test scripts/session-transcript.test.ts` uses synthetic fixtures only.

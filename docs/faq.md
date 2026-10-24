# FAQ

**Does it call a hosted API?** No. Models run locally on CPU; remote embedding providers are opt-in and need their own keys.

**How do two agents share memory but keep private notes?** Give each a private scope and both a shared scope (`POST /v1/grants`). Search reads every scope the caller may read; nothing else.

**What happens when two agents update the same memory?** The second writer gets `version_conflict` with the current version; nothing merges silently, every accepted version stays in history.

**How do I correct a fact?** Ingest the correction with `supersedes: [oldId]`. The old fact stops ranking and `explain` shows what replaced it.

**What does delete remove?** Text, vectors and graph edges. A content-free tombstone stays, so the id cannot come back.

**Can I change the embedding model later?** Yes: `hyperag models set embedding=<spec>`. A new index is built before the switch; the old one stays.

**What does it not do yet?** See [status](status.md).

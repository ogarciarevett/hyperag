# Preserve source records alongside imported memories

The semantic importer carries active observations, history and selected graph relations.
The supplemental archive preserves the logical SQLite rows it omits: prompts, sessions,
relation judgments, sync bookkeeping, deletion markers and additional observation fields.
It also retains schemas and snapshot manifests. It never executes archived sync actions.

Run the existing observation import first. After validating a read-only SQLite backup
in DELETE journal mode, run `bun scripts/engram-archive.ts --db /path/to/snapshot.db --apply`
with `HYPERAG_TOKEN` supplied privately in the environment. The endpoint is fixed to
`http://127.0.0.1:7481` and requires the dedicated `personal-import` principal.

Each source row becomes one or more immutable `engram_archive` memories in the separate
`project:engram-archive` scope. Content hashes make reruns skip existing data, and every
part is read back and checked. Table manifests link row identities to their hashes;
the root manifest links table manifests and schemas. Old snapshots remain accessible.
BLOBs use tagged base64, and integers outside JavaScript's exact range use tagged decimals.
Chunks contain at most 80,000 Unicode code points. Empty tables retain their schema.

Derived virtual/FTS shadow tables are listed as excluded: their content duplicates base
rows or stores index internals. Their definitions remain in the archived SQLite schema.
This preserves source data for recovery; it does not implement Engram's sync, ranking,
retention or conflict-judgment behavior. Source-deleted rows remain explicitly archived
records rather than deletion requests against existing memories. Raw archive text is
searchable to principals granted the archive scope; it includes historical source data.

Validation: `bun test scripts/engram-import.test.ts scripts/engram-archive.test.ts`.
The tests use generated fixtures only; no private source data belongs in this repository.

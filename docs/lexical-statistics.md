# Maintained lexical statistics (Rust storage)

Migration 0004 adds derived current-document TF/length, corpus counts/length sums, and document frequencies. TF uses stored tsvector positions and length counts unique lexemes, matching the existing BM25 implementation. Counts are keyed by scope and normalized valid-time interval; future consumers must filter readable scopes and the requested valid time. Source memories, versions, models and the default scorer are unchanged.

Source writes update the cache transactionally, including status/current-version changes, scope/validity edits and deletion. A statement-level advisory transaction lock serializes metadata writes before source row locks so counter updates and backfill cannot race. Reads use MVCC and take no writer lock. This trades write concurrency for consistent derived counters; measure write cost before adopting it for a busy writer workload.

Existing data is not scanned or backfilled automatically at startup. Call `SELECT lexical_cache_backfill(64)` repeatedly until it returns zero, using one gated batch at a time. Accepted batch sizes are non-null integers 1–256. New writes are tracked during backfill; `settings.lexical_cache_ready` becomes `true` only when current-row coverage is complete. Empty new databases start ready. Reapplying the migration and refreshing a cached document are idempotent.

Administrative TRUNCATE marks the cache `reset`; the next explicit backfill resets derived tables and rebuilds them. Until readiness is true, a consuming indexed scorer must fall back to legacy scoring and check readiness in the same snapshot as its scoring query. This PR supplies storage only; indexed scoring and stage timing are separate work units.

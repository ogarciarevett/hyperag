# Draft issue for memvid/memvid (not filed)

**Title:** Vector search returns the wrong frames after `put_many(embeddings=...)` when a document is split into pages

**Version:** memvid-sdk 2.0.160, macOS 27 arm64, Python 3.12.

**What happens.** With `create(path, enable_vec=True)` and `put_many(requests, embeddings=[...])` (one vector per request, as the README's external-embeddings section shows), `find(mode="sem", query_embedding=v_i)` should return request `i` first when the query vector is exactly request `i`'s vector. It does for most requests, but not when one request is long enough to be stored as several page frames (`uri#page-N`):

```
after commit:      111111111111111111111111111000 33 frames
after seal + open: 000000000000000000000000000111
```

30 requests, 33 frames (request 28 became pages). After `commit()` the last 3 requests are never found; after `seal()` and reopening, only the last 3 are found. It looks as if vectors are assigned to frames by position, and the extra page frames shift them.

**Repro.** `notes/memvid_vector_repro.py <BEIR SciFact corpus.jsonl>` (random unit vectors, so the embedder plays no part). Documents 0 to 29 of BEIR SciFact.

**Related, same version:**

- `put_many(..., embedder=...)` stores vectors only for documents that are split into chunks; with only single-frame documents, `find(mode="sem")` raises `MV011: Vector index is not enabled`.
- The built-in local `embedding_model="bge-small"` raises `MV015 ... requires the 'fastembed' feature which is not available on this platform` on macOS arm64, though the README lists local embeddings for macOS.
- `put_many` with 512 requests never returns (the main thread parks in the core); batches of 100 work.
- The ids returned by `put_many` do not match the `frame_id` that `find()` returns for the same document (off by one in our runs).
- Some lexical queries raise `MV004: Lexical index is not enabled` on an index that answers other queries, e.g. "Activation of PPM1D suppresses p53 function." on SciFact.

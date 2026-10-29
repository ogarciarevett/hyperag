# TypeScript local EmbeddingGemma 2 profile

Source contract for [#67](https://github.com/ogarciarevett/hyperag/issues/67).
`createLocalEmbedder({ profile: "gemma2" })` describes the same pinned 768-dimensional q8 text profile
as the Rust CLI: `onnx-community/embeddinggemma-2-ONNX` revision
`daa72c51243991dfcaf9f9137d2c573d8f7790c0`. It reads `sentence_embedding`
through AutoModel, normalizes the projected output and disables vision/audio
encoders. The installed Transformers.js 4.3.1 generic feature-extraction pipeline
does not select that output; a model-name swap is insufficient.

Query prefix: `task: search result | query: `. Document prefix:
`title: none | text: `. They are applied once. The default CPU profile uses four
intra-op threads and batches at most 16 texts. Options are schema-validated before
loading; invalid count, dimensions, non-finite or non-unit output is refused.
`createLocalEmbedder({ profile: "e5-small" })` retains the exact legacy descriptor.
The zero-argument default also stays E5: changing it would silently select an
empty candidate space for existing TypeScript callers. The service lacks a
persisted active-profile resolver. [#68](https://github.com/ogarciarevett/hyperag/issues/68)
owns empty-store Gemma selection, existing-store preservation and a separate
promotion operation; the requested new-store default is not complete here.
Model, revision, dimensions and preprocessing feed the existing vector-space ID;
the new profile cannot overwrite the legacy space under its ID.

The public ONNX export is Apache-2.0. Its pinned q8 graph SHA-256 is
`d06edd601f851c633a2519304cbeb8dc6170d7ceb61b436625c17fb9b6e74953`;
external data SHA-256 is
`278a7ff1248c3618e4bd11a607fc54f7bdc7778854230f3956d3f86bd9db4f3b`.
See the [export card](https://huggingface.co/onnx-community/embeddinggemma-2-ONNX)
and [canonical card](https://huggingface.co/google/embeddinggemma-2).

## Cached tokenizer compatibility

Transformers.js 4.3.1 discovers tokenizer metadata without forwarding the requested
revision. When both tokenizer JSON files already exist under the exact pinned
model/revision cache directory, the adapter passes that snapshot directory to
AutoTokenizer with local-only loading. The semantic model and revision remain
unchanged; AutoConfig and AutoModel still use the model ID and revision. This
avoids creating an unpinned cache alias or changing global runtime configuration.
An incomplete snapshot falls back to the normal loader; it is not an offline
bootstrap mechanism. Cache content retains the library's existing trust boundary.

## Proof and activation gate

Offline tests use an injected external inference boundary and a separate-process
library fixture. They verify configuration, text-only loading, projected output,
prefixes, normalization, space separation and safe failures without downloading
weights. They do **not** prove actual inference, memory use, recall or latency.

No runtime cutover or reindex accompanies this source candidate. Before selecting
it in a running service, pin the baseline/candidate and fixed relevance set,
predeclare resource and promotion limits, load only the bounded text artifacts,
report all retrieval metrics including recall@10 plus cold/warm p50/p95, and prove
reindex coverage and rollback. Keep the old space until acceptance passes. A
failed experiment restores the prior profile; it never deletes the old vectors.

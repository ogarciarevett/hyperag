# Shared GPU transport policy

The common transport policy became active at **2026-10-24 07:16:04Z**, after the
DMR retry exited. The first covered row is `lme-oracle hyperag nemotron+nvrerank`.
The coordinator's revised queue order became active at the same safe boundary.
The ops checkpoint records the wrapper, proxy and queue process receipts.

Every subsequent system uses the same local forwarding proxy for all shared GPU
calls: the Llama generator and extraction endpoint, Nemotron embedding, and
Nemotron reranking. This includes the shared answer/judge harness, Python SDK
clients (Mem0, Cognee, Graphiti, LangMem), Hindsight's local server, and any GPU
calls made by Letta or Memvid. Letta's archival path itself does not call the LLM;
its embedding server is local. Engram also uses the shared generator through the
common answer harness. The local BGE embedder is not a shared GPU service.

For each incoming HTTP request, only an upstream `ECONNRESET` is replayed, at most
twice: three total upstream attempts, with 250ms then 500ms backoff and identical
request bytes and headers. Upstream HTTP errors are forwarded unchanged; invalid
JSON, timeouts, and other failures are not retried by this layer. Upstream
keep-alive is disabled so implicit stale-connection retries cannot exceed this
layer's bound. Responses are buffered before forwarding, including during retries.
Client cancellation is forwarded; the proxy adds no new per-request deadline.
Credentials, request bodies and private upstream error text are not logged.

After the12:55Z NV S100 attempt failed immediately following a machine pause,
an offline real-socket/SIGSTOP regression reproduced an adapter timeout while
the client was stopped. The TypeScript GPU adapter's extra120-second wall-clock
deadline is now omitted when using the common proxy; non-proxy mode retains it.
The window budget and VM cutoff remain enforced, and SDK native deadlines remain
unchanged. New Hyperag GPU row configuration records `gpuRequestDeadline`.
The failed53-answer S100 attempt is preserved without a score and needs a full
retry; this timing reproduction does not establish that the remote endpoint
itself timed out.

Each library retains its native retry policy, and the common answer/judge harness
retains its pre-existing availability recovery. Those may issue additional HTTP
requests after a proxy error; three is the bound per proxy request, not a claim
about total attempts across SDK or harness recovery. Hyperag's direct reset
wrapper is disabled when the proxy is active, preventing duplicate reset layers.
Completed row metadata records `transportRetry` for both memory and BEIR runs.

Earlier completed rows used their native policies and the common harness's
availability recovery. Hyperag's first reset recovery was added before the
06:48Z DMR retry. Those historical rows have not been retroactively rerun or
relabeled as common-proxy runs. They belong to the earlier transport cohort;
report this distinction when comparing results. Memory-run latency is not valid
for performance comparisons because of concurrency and machine pauses.

The window ended when the VM stopped on its idle timer about 13:37Z. ArguAna rerank returned the proxy's fixed transport-error body, confirming a transport failure rather than an upstream HTTP 502. All three GPU health connections timed out. The final window report preserves the four failed NFCorpus/ArguAna rows with no score. No restart or keep-alive reset was performed.

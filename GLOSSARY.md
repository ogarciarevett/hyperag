# hyperag

The language of shared memory and retrieval for agents.

## Language

**Memory**: A stored, versioned fact with text, source, author and valid time. Not RAM and not a conversation transcript.
_Avoid_: document (a source may produce several memories).

**Scope**: A named permission boundary that memories live in and grants apply to. Not a variable scope.

**Principal**: An authenticated caller (an agent or a person) that holds grants. Not a school head and not a security officer.

**Grant**: A `read` or `write` permission from a principal on a scope, checked on every call so revocation takes effect at once.

**Slot**: A replaceable module position (store, embedding, re-ranking, graph) configured independently. Not a time slot.

**Embedding space**: The vector index tied to one embedding model, revision and preprocessing; two models never share one.

**Valid time**: When a fact held in the world (`validFrom`, `validTo`); distinct from `recordedAt`, when hyperag learned it.

**Supersede**: Mark an old memory's current version as replaced by a correction, keeping history. Not delete.
_Avoid_: overwrite.

**Invalidate**: Mark a memory's version as no longer true without a replacement.

**Context**: The cited result of retrieval for a query, with numbered citations or an `insufficientEvidence` signal.
_Avoid_: prompt (the answer model is the caller's).

**Abstention**: Returning no answer because the best evidence is below the configured score floor.

**Plugin**: The `hyperag/plugin` entry point through which a host application embeds hyperag.

**Contract**: A DTO or schema: the shape of a request, response, record or message that crosses a boundary. Not a smart contract.
_Avoid_: using "contract" for an on-chain program, or for a promise of behavior (say "guarantee"). HTTP and MCP request and response shapes are contracts.

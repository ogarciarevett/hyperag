import type { Retrieval } from "./core.js";

export type Citation = Readonly<{
  n: number;
  memoryId: string;
  version: number;
  source: string;
  author: string;
  scope: string;
}>;

export type Context = Readonly<{
  text: string;
  citations: readonly Citation[];
  tokens: number;
  /** Retrieved items that did not fit the budget. */
  dropped: number;
}>;

// shortcut: tokens are estimated as ceil(chars / 4), not counted by the generator's tokenizer;
// pass a real tokenizer here when a generator with a hard context limit is wired in.
export const estimateTokens = (text: string) => Math.ceil(text.length / 4);

/**
 * Packs retrieved passages, best first, into `maxTokens`. Each passage is numbered `[n]` and its
 * citation names the memory, version, source and author. A passage that does not fit whole is left
 * out, never cut, so a citation always points at text the reader saw in full.
 */
export function buildContext(retrieval: Retrieval, maxTokens: number): Context {
  const parts: string[] = [];
  const citations: Citation[] = [];
  let tokens = 0;
  if (retrieval.insufficientEvidence) {
    const note = "No sufficient evidence was found for this question.";
    return { text: note, citations, tokens: estimateTokens(note), dropped: retrieval.items.length };
  }
  for (const item of retrieval.items) {
    const n = citations.length + 1;
    const m = item.memory;
    const part = `[${n}] (${m.source}, ${m.author}, v${m.version}) ${m.text}`;
    const cost = estimateTokens(part) + 1;
    if (tokens + cost > maxTokens) continue;
    parts.push(part);
    tokens += cost;
    citations.push({ n, memoryId: m.id, version: m.version, source: m.source, author: m.author, scope: m.scope });
  }
  return { text: parts.join("\n"), citations, tokens, dropped: retrieval.items.length - citations.length };
}

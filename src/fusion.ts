import type { Hit } from "./types.js";

/**
 * RECIPROCAL RANK FUSION, from the original agent memory: each list adds 1 / (k + rank), so an item
 * two channels rank beats an item one channel ranks a little higher. Ties keep the newer item first,
 * as the original does.
 */
export function fuse(lists: ReadonlyArray<readonly Hit[]>, k: number): Hit[] {
  const scores = new Map<string, Hit>();
  for (const list of lists)
    list.forEach((hit, index) => {
      const prev = scores.get(hit.id);
      scores.set(hit.id, {
        id: hit.id,
        version: prev?.version || hit.version,
        at: Math.max(prev?.at ?? 0, hit.at),
        score: (prev?.score ?? 0) + 1 / (k + index + 1),
      });
    });
  return [...scores.values()].sort((a, b) => b.score - a.score || b.at - a.at || a.id.localeCompare(b.id));
}

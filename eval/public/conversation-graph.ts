/**
 * Graph edges for conversation benchmarks (LoCoMo, LongMemEval, MSC), which carry no relations.
 * No model is used; two exact rules, applied in ingest order:
 *
 * - NEXT_TURN: each turn links to the turn before it in the same session.
 * - SAME_ENTITY: each turn links to the latest earlier turn, in any session of the same
 *   conversation, that names the same entity. An entity is a capitalised word of three or more
 *   letters that follows a lowercase letter and a space, with an optional comma between them (so a
 *   sentence's first word never counts), compared exactly, minus the conversation's speaker names, months and weekdays.
 *
 * Both rules make chains, never stars: a turn gets at most one edge per entity, so a common name
 * cannot turn into a hub that two hops spread over the whole conversation.
 */
export type Turn = Readonly<{ id: string; text: string; speaker?: string }>;
export type Edge = Readonly<{
  from: string;
  to: string;
  type: "NEXT_TURN" | "SAME_ENTITY";
}>;

const CALENDAR = new Set(
  "January February March April May June July August September October November December Monday Tuesday Wednesday Thursday Friday Saturday Sunday".split(
    " ",
  ),
);

export function entities(
  text: string,
  speakers: ReadonlySet<string>,
): Set<string> {
  const found = new Set<string>();
  for (const m of text.matchAll(/(?<=\p{Ll},? )\p{Lu}\p{Ll}{2,}(?!\p{L})/gu))
    if (!CALENDAR.has(m[0]) && !speakers.has(m[0])) found.add(m[0]);
  return found;
}

/** State of one conversation across its sessions: speakers seen and the latest turn per entity. */
export type Conversation = {
  speakers: Set<string>;
  lastByEntity: Map<string, string>;
};
export const newConversation = (): Conversation => ({
  speakers: new Set(),
  lastByEntity: new Map(),
});

/** Edges for one session, in turn order; updates the conversation state. */
export function sessionEdges(c: Conversation, turns: readonly Turn[]): Edge[] {
  for (const t of turns)
    if (t.speaker) for (const w of t.speaker.split(/\s+/u)) c.speakers.add(w);
  const edges: Edge[] = [];
  turns.forEach((t, i) => {
    if (i > 0)
      edges.push({ from: t.id, to: turns[i - 1]!.id, type: "NEXT_TURN" });
    for (const e of entities(t.text, c.speakers)) {
      const last = c.lastByEntity.get(e);
      if (last !== undefined && last !== t.id)
        edges.push({ from: t.id, to: last, type: "SAME_ENTITY" });
      c.lastByEntity.set(e, t.id);
    }
  });
  return edges;
}

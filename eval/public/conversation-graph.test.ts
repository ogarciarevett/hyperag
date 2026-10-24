import { expect, test } from "bun:test";

import { entities, newConversation, sessionEdges } from "./conversation-graph";

test("entities: mid-sentence capitalised words, minus speakers, calendar and sentence starts", () => {
  const speakers = new Set(["Caroline", "Melanie"]);
  const text = `8:56 pm on 20 July, 2023: Caroline said, "Yesterday I met Melanie and her dog Oscar at the Sunset park, then went to Paris on Monday."`;
  expect([...entities(text, speakers)].sort()).toEqual([
    "Oscar",
    "Paris",
    "Sunset",
  ]);
  expect(entities("The meeting moved. Then we left.", speakers).size).toBe(0);
  expect([...entities("we saw José and Zoë", speakers)]).toEqual([
    "José",
    "Zoë",
  ]);
});

test("sessionEdges: next turn within a session, same entity as a chain across sessions", () => {
  const c = newConversation();
  const s1 = sessionEdges(c, [
    {
      id: "D1:1",
      speaker: "Caroline",
      text: "Caroline said, I adopted a cat named Luna.",
    },
    { id: "D1:2", speaker: "Melanie", text: "Melanie said, that is lovely." },
  ]);
  expect(s1).toEqual([{ from: "D1:2", to: "D1:1", type: "NEXT_TURN" }]);
  const s2 = sessionEdges(c, [
    {
      id: "D2:1",
      speaker: "Melanie",
      text: "Melanie said, how is Luna doing?",
    },
    {
      id: "D2:2",
      speaker: "Caroline",
      text: "Caroline said, my cat Luna loves Melanie.",
    },
  ]);
  // No edge back to session 1's last turn; Luna chains D2:2 -> D2:1 -> D1:1; a speaker name is no entity.
  expect(s2).toEqual([
    { from: "D2:1", to: "D1:1", type: "SAME_ENTITY" },
    { from: "D2:2", to: "D2:1", type: "NEXT_TURN" },
    { from: "D2:2", to: "D2:1", type: "SAME_ENTITY" },
  ]);
});

test("sessionEdges: a resumed run (fresh state, same sessions) gives the same edges", () => {
  const turns = [
    { id: "a", text: "we met Bob" },
    { id: "b", text: "then Bob left" },
  ];
  const first = sessionEdges(newConversation(), turns);
  expect(first).toEqual([
    { from: "b", to: "a", type: "NEXT_TURN" },
    { from: "b", to: "a", type: "SAME_ENTITY" },
  ]);
  expect(sessionEdges(newConversation(), turns)).toEqual(first);
});

import { expect, test } from "bun:test";
import { Pool } from "pg";

import { createPostgresStore } from "../src/adapters/postgres";
import { Hyperag } from "../src/core";
import { freshDatabase } from "./helpers";

test("a 20000-character memory round-trips intact", async () => {
  const db = await freshDatabase();
  const pool = new Pool({ connectionString: db.url, max: 1 });
  try {
    const h = new Hyperag({ store: createPostgresStore(pool), admins: ["admin"] });
    await h.init();
    for (const permission of ["read", "write"] as const)
      await h.grant("admin", "writer", "long-text", permission);
    const text = "roundtrip ".repeat(2000);
    expect(text.length).toBe(20000);
    const saved = await h.ingest("writer", { scope: "long-text", source: "synthetic", text });
    expect((await h.explain("writer", saved.id)).versions[0]?.text).toBe(text);
  } finally {
    await pool.end();
    await db.drop();
  }
});

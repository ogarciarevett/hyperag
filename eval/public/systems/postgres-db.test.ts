import { expect, test } from "bun:test";
import type { Pool } from "pg";
import { benchmarkDatabaseName, ensureBenchmarkDatabase } from "./postgres-db";

function admin(query: (sql: string) => Promise<unknown>): Pool {
  return { query } as unknown as Pool;
}

for (const code of ["23505", "42P04"]) {
  test(`parallel creation recovers ${code} only after checking ownership`, async () => {
    let created = false;
    let checks = 0;
    const connection = admin(async (sql) => {
      if (sql.startsWith("select")) {
        checks++;
        return { rowCount: created ? 1 : 0, rows: created ? [{ owned: true }] : [] };
      }
      if (created) throw Object.assign(new Error("concurrent create"), { code });
      created = true;
      return { rowCount: null, rows: [] };
    });
    await Promise.all([ensureBenchmarkDatabase(connection, "bench_test"), ensureBenchmarkDatabase(connection, "bench_test")]);
    expect(created).toBe(true);
    expect(checks).toBe(3);
  });
}

test("an existing database owned by another role is refused", async () => {
  await expect(ensureBenchmarkDatabase(admin(async () => ({ rowCount: 1, rows: [{ owned: false }] })), "bench_test"))
    .rejects.toThrow("belongs to another role");
});

for (const owned of [false, undefined]) {
  test(`duplicate create still fails when the database is ${owned === false ? "foreign" : "missing"}`, async () => {
    const error = Object.assign(new Error("duplicate"), { code: "23505" });
    let checks = 0;
    const connection = admin(async (sql) => {
      if (!sql.startsWith("select")) throw error;
      checks++;
      return { rowCount: checks > 1 && owned !== undefined ? 1 : 0, rows: checks > 1 && owned !== undefined ? [{ owned }] : [] };
    });
    await expect(ensureBenchmarkDatabase(connection, "bench_test")).rejects.toBe(error);
  });
}

test("other creation failures are preserved", async () => {
  const error = Object.assign(new Error("permission denied"), { code: "42501" });
  await expect(ensureBenchmarkDatabase(admin(async (sql) => {
    if (sql.startsWith("select")) return { rowCount: 0, rows: [] };
    throw error;
  }), "bench_test")).rejects.toBe(error);
});

test("local CI selects dev databases while benchmark defaults retain their names", () => {
  expect(benchmarkDatabaseName("Demo/x", "bm25", "dev_")).toBe("dev_demo_x_bm25");
  expect(benchmarkDatabaseName("Demo/x", "bm25", "pub_")).toBe("pub_demo_x_bm25");
  expect(() => benchmarkDatabaseName("x", "bm25", 'bad";')).toThrow("invalid benchmark database prefix");
});

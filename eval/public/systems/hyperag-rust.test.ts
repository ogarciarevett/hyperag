import { expect, test } from "bun:test";

// Run the real adapter with an isolated database mock; never launch a service or touch Postgres.
test("Rust adapter refuses a foreign-owned database before service startup", async () => {
  const script = `
    import { mock } from "bun:test";
    let ended = false;
    mock.module("pg", () => ({ Pool: class {
      async query() { return { rowCount: 1, rows: [{ owned: false }] }; }
      async end() { ended = true; }
    } }));
    process.env.HYPERAG_BIN = "/nonexistent-hyperag-ownership-regression";
    const { default: factory } = await import(${JSON.stringify(new URL("./hyperag-rust.ts", import.meta.url).href)});
    const system = await factory("bm25");
    try {
      await system.open("ownership_test/conversation");
      throw new Error("foreign database accepted");
    } catch (error) {
      if (!error.message.includes("belongs to another role") || !ended) throw error;
    }
  `;
  const child = Bun.spawn([process.execPath, "-e", script], { stdout: "ignore", stderr: "pipe" });
  const [code, error] = await Promise.all([child.exited, new Response(child.stderr).text()]);
  if (code) console.error(error);
  expect(code).toBe(0);
});

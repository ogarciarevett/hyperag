import { expect, test } from "bun:test";
import { fileURLToPath } from "node:url";

test("plugin fixture disconnects before force-dropping its database", async () => {
  const child = Bun.spawn(
    [
      process.execPath,
      "test",
      "--preload",
      fileURLToPath(
        new URL("./fixtures/slow-pg-disconnect.ts", import.meta.url),
      ),
      fileURLToPath(new URL("./plugin.test.ts", import.meta.url)),
      "--timeout",
      "30000",
    ],
    {
      cwd: fileURLToPath(new URL("../", import.meta.url)),
      env: { ...process.env, HF_HUB_OFFLINE: "1", TRANSFORMERS_OFFLINE: "1" },
      stdout: "pipe",
      stderr: "pipe",
      timeout: 35_000,
    },
  );
  const [exit, stdout, stderr] = await Promise.all([
    child.exited,
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
  ]);
  // Assert on the symptom without dumping connection details from a child error.
  expect(
    (stdout + stderr).includes(
      "terminating connection due to administrator command",
    ),
  ).toBe(false);
  expect(exit).toBe(0);
}, 40_000);

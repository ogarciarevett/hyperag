import { expect, test } from "bun:test";
import { EventEmitter } from "node:events";
import { closeFixturePool } from "../examples/readme/close-pool";

class FixturePool extends EventEmitter {
  endCalls = 0;
  constructor(readonly totalCount: number) { super(); }
  async end() { this.endCalls++; }
}

test("database teardown waits for every pool disconnect after end resolves", async () => {
  const pool = new FixturePool(2);
  const order: string[] = [];
  const observer = () => order.push("disconnect");
  pool.on("remove", observer);
  const closing = closeFixturePool(pool).then(() => order.push("drop database"));
  // Flush the already-resolved end promise; disconnects are controlled explicitly.
  await Promise.resolve();
  await Promise.resolve();
  expect(order).toEqual([]);
  pool.emit("remove");
  await Promise.resolve();
  expect(order).toEqual(["disconnect"]);
  pool.emit("remove");
  await closing;
  expect(order).toEqual(["disconnect", "disconnect", "drop database"]);
  expect(pool.listeners("remove")).toEqual([observer]);
});

test("an empty pool still ends without waiting for nonexistent disconnects", async () => {
  const pool = new FixturePool(0);
  await closeFixturePool(pool);
  expect(pool.endCalls).toBe(1);
  expect(pool.listenerCount("remove")).toBe(0);
});

test("end failure propagates without waiting for disconnects or retaining a listener", async () => {
  const pool = new FixturePool(2);
  const failure = new Error("fixture close failed");
  pool.end = async () => { throw failure; };
  await expect(closeFixturePool(pool)).rejects.toBe(failure);
  expect(pool.listenerCount("remove")).toBe(0);
});

test("synchronous disconnect events during end are not missed", async () => {
  const pool = new FixturePool(2);
  pool.end = async () => { pool.emit("remove"); pool.emit("remove"); };
  await closeFixturePool(pool);
  expect(pool.listenerCount("remove")).toBe(0);
});

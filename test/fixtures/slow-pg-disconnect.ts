import { mock } from "bun:test";
import { fileURLToPath } from "node:url";
import * as pg from "pg";

// Exercise the real plugin fixture with a slow, asynchronous client disconnect.
// Pool.end() must not be treated as proof that its clients have disconnected.
class SlowClient extends pg.Client {
  override end(): Promise<void>;
  override end(callback: (error: Error) => void): void;
  override end(callback?: (error: Error) => void): Promise<void> | void {
    if (callback) {
      setTimeout(() => super.end(callback), 100);
      return;
    }
    return new Promise<void>((resolve, reject) => {
      setTimeout(() => super.end().then(resolve, reject), 100);
    });
  }
}

class SlowPool extends pg.Pool {
  constructor(options?: pg.PoolConfig) {
    super({ ...options, Client: SlowClient });
  }
}

mock.module(fileURLToPath(import.meta.resolve("pg")), () => ({ ...pg, Pool: SlowPool }));

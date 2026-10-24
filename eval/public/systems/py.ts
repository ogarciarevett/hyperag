import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

import type { MemorySystem, SystemFactory } from "../system";

/**
 * A Python adapter (systems/<name>.py, served by pyserve.py) as a MemorySystem. It runs in the
 * system's own venv, eval/public/.venv-<name>, so competitors keep their pinned dependencies apart.
 */
export function pyAdapter(name: string): SystemFactory {
  return async (profile) => {
    const python = `${import.meta.dir}/../.venv-${name}/bin/python`;
    const child = spawn(python, ["-I", `${import.meta.dir}/pyserve.py`, name, profile], { stdio: ["pipe", "pipe", "inherit"] });
    const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
    // the adapter answers one request at a time, so calls queue behind each other
    let queue: Promise<unknown> = Promise.resolve();
    let nextId = 0;
    let broken: Error | undefined;
    const send = async <T>(method: string, args: unknown[]): Promise<T> => {
      if (broken) throw broken;
      const id = ++nextId;
      child.stdin.write(`${JSON.stringify({ id, method, args })}\n`);
      const { value, done } = await lines.next();
      if (done) throw (broken = new Error(`${name} adapter exited`));
      let r: { id?: number; result?: T; error?: string };
      try {
        r = JSON.parse(value);
      } catch {
        throw (broken = new Error(`${name} adapter protocol broken: ${value.slice(0, 200)}`));
      }
      // a reply for another request means the stream is out of step: stop the run, never pair wrong answers
      if (r.id !== id) throw (broken = new Error(`${name} adapter protocol out of step (reply ${r.id} for request ${id})`));
      if (r.error !== undefined) throw new Error(r.error);
      return r.result as T;
    };
    const call = <T>(method: string, ...args: unknown[]): Promise<T> => {
      const p = queue.then(() => send<T>(method, args));
      queue = p.catch(() => undefined);
      return p;
    };
    const system: MemorySystem = {
      describe: () => call("describe"),
      open: (ns) => call("open", ns),
      ingest: (ns, docs) => call("ingest", ns, docs),
      search: (ns, query, k, opts) => call("search", ns, query, k, opts ?? null),
      rssBytes: () => call("rss"),
      close: async () => {
        await call("close");
        child.stdin.end();
      },
    };
    return system;
  };
}

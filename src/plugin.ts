import { constants } from "node:fs";
import { open } from "node:fs/promises";

import type { Context } from "./context.js";
import type { Retrieval, RetrieveOptions } from "./core.js";
import type { NewMemory, WriteResult } from "./types.js";

export class PluginError extends Error {
  constructor(readonly code: string, readonly status = 0) {
    super(`Hyperag plugin: ${code}`);
    this.name = "PluginError";
  }
}

/** Load a remote client; the service remains the authority for identity and scope grants. */
export async function loadPlugin(config: { url: string; tokenFile: string; timeoutMs?: number }) {
  let base: URL;
  try {
    base = new URL(config.url);
    if (base.username || base.password || base.search || base.hash || base.pathname !== "/" ||
        !(base.protocol === "https:" || (base.protocol === "http:" && ["127.0.0.1", "[::1]", "localhost"].includes(base.hostname))))
      throw new Error();
  } catch { throw new PluginError("invalid_url"); }
  const timeout = config.timeoutMs ?? 30_000;
  if (!Number.isInteger(timeout) || timeout < 1 || timeout > 300_000) throw new PluginError("invalid_timeout");
  let token: string;
  try {
    const file = await open(config.tokenFile, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    try {
      const stat = await file.stat();
      if (!stat.isFile() || stat.size < 1 || stat.size > 4096 || (stat.mode & 0o077) !== 0 || stat.uid !== process.getuid?.()) throw new Error();
      const buffer = Buffer.alloc(4097);
      const { bytesRead } = await file.read(buffer, 0, buffer.length, 0);
      token = buffer.subarray(0, bytesRead).toString("utf8").trim();
      if (bytesRead !== stat.size || !/^[A-Za-z0-9._~+/-]+=*$/u.test(token)) throw new Error();
    } finally { await file.close(); }
  } catch { throw new PluginError("invalid_token_file"); }

  async function call<T>(path: string, body?: unknown): Promise<T> {
    try {
      const response = await fetch(new URL(path, base), {
        method: body === undefined ? "GET" : "POST",
        headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
        redirect: "error",
        signal: AbortSignal.timeout(timeout),
      });
      if (!response.ok) {
        await response.body?.cancel();
        throw new PluginError("http_error", response.status);
      }
      return await response.json() as T;
    } catch (error) {
      if (error instanceof PluginError) throw error;
      throw new PluginError("request_failed");
    }
  }
  return Object.freeze({
    save: (memory: NewMemory) => call<WriteResult>("/v1/memories", memory),
    recall: (query: string, options: RetrieveOptions = {}) => call<Retrieval>("/v1/retrieve", { ...options, query }),
    search: (query: string, options: RetrieveOptions & { maxTokens?: number } = {}) => call<Context & { retrieval: Retrieval }>("/v1/context", { ...options, query }),
    health: () => call<{ principal: string }>("/v1/whoami"),
  });
}

import { createHash, timingSafeEqual } from "node:crypto";

import type { Hyperag } from "./core.js";
import { HyperagError } from "./types.js";

/**
 * THE TRUST BOUNDARY. A caller proves who it is with a bearer token; the service maps the token's
 * SHA-256 to a principal from its own registry and never reads an identity from the request body.
 * What that principal may read or write comes from the grants table; the core checks admins.
 */
export type ServiceAuth = Readonly<{
  /** sha256(token) hex -> principal id. Plain tokens are never stored. */
  tokens: Readonly<Record<string, string>>;
}>;

const STATUS: Record<HyperagError["code"], number> = {
  forbidden: 403,
  not_found: 404,
  version_conflict: 409,
  already_exists: 409,
  invalid_input: 400,
  not_active: 409,
  embedding_space_mismatch: 409,
};
const MAX_BODY_BYTES = 1_048_576;

export const tokenHash = (token: string) => createHash("sha256").update(token).digest("hex");

export function createHandler(hyperag: Hyperag, auth: ServiceAuth) {
  const hashes = Object.entries(auth.tokens).map(([hash, principal]) => [Buffer.from(hash, "hex"), principal] as const);

  function principalOf(req: Request): string | null {
    const header = req.headers.get("authorization") ?? "";
    if (!header.startsWith("Bearer ")) return null;
    const presented = Buffer.from(tokenHash(header.slice(7)), "hex");
    const match = hashes.find(([hash]) => hash.length === presented.length && timingSafeEqual(hash, presented));
    return match?.[1] ?? null;
  }

  async function body(req: Request): Promise<Record<string, unknown>> {
    if (Number(req.headers.get("content-length") ?? 0) > MAX_BODY_BYTES)
      throw new HyperagError("invalid_input", "body too large");
    const text = await req.text();
    if (Buffer.byteLength(text) > MAX_BODY_BYTES) throw new HyperagError("invalid_input", "body too large");
    try {
      const parsed = JSON.parse(text || "{}") as unknown;
      if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error();
      return parsed as Record<string, unknown>;
    } catch {
      throw new HyperagError("invalid_input", "body must be a JSON object");
    }
  }

  const str = (b: Record<string, unknown>, key: string): string => {
    const v = b[key];
    if (typeof v !== "string" || v.length === 0) throw new HyperagError("invalid_input", `${key} is required`);
    return v;
  };
  const optStr = (b: Record<string, unknown>, key: string): string | undefined => {
    if (b[key] === undefined) return undefined;
    return str(b, key);
  };
  const int = (b: Record<string, unknown>, key: string, min: number, max: number): number | undefined => {
    const v = b[key];
    if (v === undefined) return undefined;
    if (typeof v !== "number" || !Number.isInteger(v) || v < min || v > max)
      throw new HyperagError("invalid_input", `${key} must be an integer from ${min} to ${max}`);
    return v;
  };
  const record = (b: Record<string, unknown>, key: string): Record<string, unknown> | undefined => {
    const v = b[key];
    if (v === undefined) return undefined;
    if (v === null || typeof v !== "object" || Array.isArray(v)) throw new HyperagError("invalid_input", `${key} must be an object`);
    return v as Record<string, unknown>;
  };
  const strings = (b: Record<string, unknown>, key: string): string[] | undefined => {
    const v = b[key];
    if (v === undefined) return undefined;
    if (!Array.isArray(v) || !v.every((x) => typeof x === "string" && x.length > 0))
      throw new HyperagError("invalid_input", `${key} must be a list of ids`);
    return v as string[];
  };
  // Only known fields reach the core, each checked; undefined ones are left out.
  const defined = <T extends Record<string, unknown>>(o: T) =>
    Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;
  const version = (b: Record<string, unknown>) => ({
    text: str(b, "text"),
    source: str(b, "source"),
    validFrom: optStr(b, "validFrom"),
    validTo: optStr(b, "validTo"),
    metadata: record(b, "metadata"),
    idempotencyKey: optStr(b, "idempotencyKey"),
  });
  const newMemory = (b: Record<string, unknown>) =>
    defined({ ...version(b), scope: str(b, "scope"), kind: optStr(b, "kind"), id: optStr(b, "id"), supersedes: strings(b, "supersedes") });
  const memoryUpdate = (b: Record<string, unknown>) =>
    ({ ...defined(version(b)), expectedVersion: required(int(b, "expectedVersion", 1, 2 ** 31 - 1), "expectedVersion") });
  const required = <T>(v: T | undefined, key: string): T => {
    if (v === undefined) throw new HyperagError("invalid_input", `${key} is required`);
    return v;
  };
  const retrieveOptions = (b: Record<string, unknown>) => {
    const channels = record(b, "channels");
    if (channels && !Object.values(channels).every((v) => typeof v === "boolean"))
      throw new HyperagError("invalid_input", "channels values must be booleans");
    return defined({
      limit: int(b, "limit", 1, 50),
      scopes: strings(b, "scopes"),
      excludeScopes: strings(b, "excludeScopes") ?? strings(b, "exclude_scopes"),
      candidatesPerChannel: int(b, "candidatesPerChannel", 1, 150),
      maxTokens: int(b, "maxTokens", 1, 32_000),
      asOf: optStr(b, "asOf"),
      channels: channels as Record<"bm25", boolean> | undefined,
    });
  };
  const perm = (b: Record<string, unknown>) => {
    const p = str(b, "permission");
    if (p !== "read" && p !== "write") throw new HyperagError("invalid_input", "permission is read or write");
    return p;
  };

  async function route(req: Request, who: string): Promise<unknown> {
    const url = new URL(req.url);
    // Ids with a scope prefix contain '/', so clients percent-encode them as one segment.
    let parts: string[];
    try {
      parts = url.pathname.split("/").filter(Boolean).map(decodeURIComponent);
    } catch {
      throw new HyperagError("invalid_input", "bad path encoding");
    }
    const [v1, resource, id, action] = parts;
    if (v1 !== "v1") throw new HyperagError("not_found", "no such route");
    const m = req.method;

    if (resource === "memories" && id === undefined && m === "POST") return hyperag.ingest(who, newMemory(await body(req)));
    if (resource === "memories" && id !== undefined) {
      if (action === undefined && m === "PATCH") return hyperag.update(who, id, memoryUpdate(await body(req)));
      if (action === undefined && m === "DELETE") {
        await hyperag.delete(who, id, url.searchParams.get("reason") ?? "deleted");
        return { deleted: id };
      }
      if (action === "invalidate" && m === "POST") {
        await hyperag.invalidate(who, id, str(await body(req), "reason"));
        return { invalidated: id };
      }
      if (action === "explain" && m === "GET") return hyperag.explain(who, id);
      if (action === "relations" && m === "GET") return hyperag.relations(who, id);
    }
    if (resource === "retrieve" && m === "POST") {
      const b = await body(req);
      return hyperag.retrieve(who, str(b, "query"), retrieveOptions(b));
    }
    if (resource === "context" && m === "POST") {
      const b = await body(req);
      return hyperag.context(who, str(b, "query"), retrieveOptions(b));
    }
    if (resource === "relations" && m === "POST") {
      const b = await body(req);
      await hyperag.relate(who, { from: str(b, "from"), to: str(b, "to"), type: str(b, "type") });
      return { related: true };
    }
    if (resource === "grants" && (m === "POST" || m === "DELETE")) {
      const b = await body(req);
      if (m === "POST") await hyperag.grant(who, str(b, "principal"), str(b, "scope"), perm(b));
      else await hyperag.revoke(who, str(b, "principal"), str(b, "scope"), perm(b));
      return { ok: true };
    }
    if (resource === "scopes" && id === undefined && m === "GET") return hyperag.scopes(who);
    if (resource === "whoami" && m === "GET") return { principal: who };
    throw new HyperagError("not_found", "no such route");
  }

  return async function handle(req: Request): Promise<Response> {
    const who = principalOf(req);
    if (who === null) return Response.json({ error: { code: "unauthenticated" } }, { status: 401 });
    try {
      return Response.json(await route(req, who));
    } catch (error) {
      if (error instanceof HyperagError)
        return Response.json({ error: { code: error.code, message: error.message, ...error.detail } }, { status: STATUS[error.code] });
      console.error("hyperag request failed");
      return Response.json({ error: { code: "internal" } }, { status: 500 });
    }
  };
}

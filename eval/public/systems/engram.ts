import { engramMcp } from "../../engram-mcp";
import type { SystemFactory } from "../system";

/**
 * Engram as agents on this machine run it: its MCP server and mem_save / mem_search, one project
 * per namespace, in a throwaway data dir. Profiles: `default` (Engram's default search: every word
 * must match) and `any` (`match_mode=any`). mem_search returns at most 20 results.
 */
/** Engram project names cannot hold "/" (a `<set>/<item>` namespace). */
const project = (ns: string) => ns.replaceAll("/", "_");

const factory: SystemFactory = async (profile) => {
  if (profile !== "default" && profile !== "any") throw new Error("engram profile is default or any");
  const e = engramMcp(profile === "any" ? "any" : undefined);
  const version = Bun.spawnSync(["engram", "version"]).stdout.toString().trim();
  return {
    describe: async () => ({ system: "engram", version, profile, config: { transport: "mcp stdio", match_mode: profile === "any" ? "any" : "all (default)", asOf: "none (no date parameter at search)" } }),
    open: async () => undefined,
    ingest: async (ns, docs) => {
      for (const d of docs) await e.save(project(ns), d.id, d.title ? `${d.title}\n${d.text}` : d.text);
    },
    search: async (ns, query, k) =>
      (await e.search(project(ns), query, Math.min(k, 20))).map((r) => ({ id: r.title, text: r.text })),
    close: () => e.close(),
  };
};
export default factory;

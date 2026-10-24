import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { connectStdio } from "./mcp-client";

type Client = Awaited<ReturnType<typeof connectStdio>>;

/**
 * Engram as agents on this machine run it: the `engram mcp` server and its mem_save / mem_search /
 * mem_update / mem_delete / mem_get_observation tools, one server per project (as a session started
 * in that project gets), in a throwaway ENGRAM_DATA_DIR so the machine's real memory is untouched.
 */
export function engramMcp(matchMode: "all" | "any" | undefined) {
  const dir = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), "engram-mcp-"));
  const env = { ...process.env, ENGRAM_DATA_DIR: dir, ENGRAM_NO_UPDATE_CHECK: "1" };
  const clients = new Map<string, Promise<Client>>();
  // At most 2 servers per adapter (one per runner worker), least recently used closed first: a benchmark with 500 conversations
  // would otherwise hold 500 servers (about 30 MB each). The data dir is shared, so a closed project
  // reopens with its memories, as a new agent session would.
  const client = (project: string) => {
    let c = clients.get(project);
    if (c) clients.delete(project);
    else {
      c = connectStdio("engram", ["mcp", "--project", project], env);
      while (clients.size >= 2) {
        const [oldest, old] = clients.entries().next().value!;
        clients.delete(oldest);
        void old.then((o) => o.close());
      }
    }
    clients.set(project, c);
    return c;
  };
  const json = (text: string) => JSON.parse(text) as Record<string, unknown>;
  return {
    async save(project: string, title: string, content: string, topicKey?: string): Promise<number> {
      const r = await (await client(project)).call("mem_save", {
        title,
        content,
        type: "discovery",
        capture_prompt: false,
        ...(topicKey ? { topic_key: topicKey } : {}),
      });
      if (r.isError) throw new Error(`mem_save: ${r.text.slice(0, 200)}`);
      return Number(json(r.text).id);
    },
    async search(project: string, query: string, limit: number) {
      const r = await (await client(project)).call("mem_search", {
        query,
        limit,
        response_format: "compact",
        ...(matchMode ? { match_mode: matchMode } : {}),
      });
      if (r.isError) throw new Error(`mem_search: ${r.text.slice(0, 200)}`);
      const results = (json(r.text).results as Array<{ id: number; title: string; preview: string }> | undefined) ?? [];
      return results.map((x) => ({ id: x.id, title: x.title, text: x.preview }));
    },
    async update(project: string, id: number, content: string) {
      const r = await (await client(project)).call("mem_update", { id, content, expected_project: project });
      if (r.isError) throw new Error(`mem_update: ${r.text.slice(0, 200)}`);
    },
    async remove(project: string, id: number) {
      const r = await (await client(project)).call("mem_delete", { id, expected_project: project, hard_delete: true });
      if (r.isError) throw new Error(`mem_delete: ${r.text.slice(0, 200)}`);
    },
    async history(project: string, id: number): Promise<string[]> {
      const r = await (await client(project)).call("mem_get_observation", { id, include_history: true });
      if (r.isError) return [];
      const body = json(r.text) as { history?: Array<{ content: string }>; result?: string };
      return [...(body.history ?? []).map((h) => `prior:${h.content}`), `current:${body.result ?? ""}`];
    },
    /** Every MCP server stops, as at the end of every agent session. */
    async restart() {
      for (const c of clients.values()) (await c).close();
      clients.clear();
    },
    async close() {
      await this.restart();
      rmSync(dir, { recursive: true, force: true });
    },
  };
}

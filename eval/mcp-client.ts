import { type ChildProcessWithoutNullStreams, spawn } from "node:child_process";

/** A minimal MCP client over stdio (newline-delimited JSON-RPC), enough to call tools. */
export async function connectStdio(command: string, args: string[], env: Record<string, string | undefined>) {
  const child: ChildProcessWithoutNullStreams = spawn(command, args, { env, stdio: ["pipe", "pipe", "pipe"] });
  let buffer = "";
  let nextId = 1;
  const pending = new Map<number, (msg: { result?: unknown; error?: { message: string } }) => void>();
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk: string) => {
    buffer += chunk;
    for (let nl = buffer.indexOf("\n"); nl >= 0; nl = buffer.indexOf("\n")) {
      const line = buffer.slice(0, nl).trim();
      buffer = buffer.slice(nl + 1);
      if (!line) continue;
      const msg = JSON.parse(line) as { id?: number; result?: unknown; error?: { message: string } };
      if (msg.id !== undefined) pending.get(msg.id)?.(msg);
    }
  });
  const request = (method: string, params: unknown) =>
    new Promise<unknown>((resolve, reject) => {
      const id = nextId++;
      pending.set(id, (msg) => (msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result)));
      child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    });
  await request("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "hyperag-eval", version: "0" } });
  child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" })}\n`);
  return {
    listTools: async () => ((await request("tools/list", {})) as { tools: Array<{ name: string; inputSchema: unknown }> }).tools,
    /** The tool's text content joined, and whether the tool reported an error. */
    call: async (name: string, args: Record<string, unknown>) => {
      const r = (await request("tools/call", { name, arguments: args })) as { content?: Array<{ text?: string }>; isError?: boolean; structuredContent?: unknown };
      return { text: (r.content ?? []).map((c) => c.text ?? "").join("\n"), isError: r.isError === true, structured: r.structuredContent };
    },
    close: () => child.kill(),
  };
}

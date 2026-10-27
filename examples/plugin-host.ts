import { loadPlugin, PluginError } from "hyperag/plugin";

// The host registers these functions in its own tool system; credentials stay in a private file.
try {
  const memory = await loadPlugin({
    url: process.env.HYPERAG_URL ?? "http://127.0.0.1:7480",
    tokenFile: process.env.HYPERAG_TOKEN_FILE ?? "./agent.token",
  });
  await memory.health();
  await memory.save({ scope: "project:demo", source: "example-host", text: "The example stores decisions in Hyperag." });
  const recalled = await memory.recall("where are decisions stored?");
  const context = await memory.search("where are decisions stored?", { maxTokens: 400 });
  console.log({ recalled: recalled.items.length, citations: context.citations.length });
} catch (error) {
  console.error(error instanceof PluginError ? { code: error.code, status: error.status } : { code: "host_failed" });
  process.exitCode = 1;
}

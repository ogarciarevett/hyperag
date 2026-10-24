import { authFromEnv, hyperagFromEnv } from "./config.js";
import { createHandler } from "./server.js";

const { hyperag } = await hyperagFromEnv();
const server = Bun.serve({
  hostname: process.env.HYPERAG_HOST ?? "127.0.0.1",
  port: Number(process.env.HYPERAG_PORT ?? 7480),
  maxRequestBodySize: 1_048_576,
  fetch: createHandler(hyperag, authFromEnv()),
});
console.log(`hyperag listening on ${server.url}`);

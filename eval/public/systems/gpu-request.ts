export async function withResetRetry<T>(request: () => Promise<T>): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await request();
    } catch (error) {
      // Embedding and reranking are stateless; replay only a reset, at most twice.
      if ((error as { code?: string })?.code !== "ECONNRESET" || attempt >= 2) throw error;
      await Bun.sleep(250 * 2 ** attempt);
    }
  }
}

export async function gpuRequest(url: string, path: string, body: unknown, key?: string): Promise<Record<string, unknown>> {
  const payload = JSON.stringify(body);
  const request = async () => {
    const r = await fetch(`${url.replace(/\/+$/u, "")}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json", ...(key ? { authorization: `Bearer ${key}` } : {}) },
      body: payload,
      // A SIGSTOP machine pause consumes wall-clock deadlines; the shared proxy adds none.
      signal: process.env.GPU_RETRY_PROXY === "1" ? undefined : AbortSignal.timeout(120_000),
      keepalive: false,
    });
    if (!r.ok) {
      // Classify the proxy's fixed error body without exposing upstream error text.
      const transport = process.env.GPU_RETRY_PROXY === "1" && r.status === 502 &&
        await r.text() === "GPU transport failed after bounded recovery";
      throw new Error(`gpu endpoint ${path}: ${r.status}${transport ? " (common proxy transport failure)" : " (upstream HTTP response)"}`);
    }
    return await r.json() as Record<string, unknown>;
  };
  // The common proxy already applies this policy to all systems; never multiply it here.
  return process.env.GPU_RETRY_PROXY === "1" ? request() : withResetRetry(request);
}

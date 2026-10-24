/** RAM of this process's descendants (servers an adapter spawned, such as Engram's MCP server), for
 * adapters without their own rssBytes(); the runner's own RSS is recorded separately. */
export function childrenRssBytes(): number {
  const rows = Bun.spawnSync(["ps", "-A", "-o", "pid=,ppid=,rss="]).stdout.toString().trim().split("\n").map((l) => l.trim().split(/\s+/u).map(Number));
  const kids = new Map<number, Array<[number, number]>>();
  for (const [pid, ppid, rss] of rows) kids.set(ppid!, [...(kids.get(ppid!) ?? []), [pid!, rss!]]);
  let total = 0;
  const todo = [process.pid];
  while (todo.length) for (const [pid, rss] of kids.get(todo.pop()!) ?? []) (total += rss), todo.push(pid);
  return total * 1024;
}

/** RAM in use by docker containers (docker stats), for systems whose store runs in a container. */
export function containerBytes(names: readonly string[]): number {
  if (!names.length) return 0;
  const docker = process.env.DOCKER ?? "docker";
  const out = Bun.spawnSync([docker, "stats", "--no-stream", "--format", "{{.MemUsage}}", ...names]).stdout.toString();
  const units: Record<string, number> = { KiB: 2 ** 10, MiB: 2 ** 20, GiB: 2 ** 30, B: 1 };
  let total = 0;
  for (const line of out.trim().split("\n").filter(Boolean)) {
    const used = line.split("/")[0]!.trim();
    const unit = Object.keys(units).find((u) => used.endsWith(u))!;
    total += Number(used.slice(0, -unit.length)) * units[unit]!;
  }
  return Math.round(total);
}

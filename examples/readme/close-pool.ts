// pg-pool 3.14 can resolve end() before its public remove events finish.
// Keep temporary database teardown behind actual client disconnect completion.
export async function closeFixturePool(pool: {
  readonly totalCount: number;
  end(): Promise<void>;
  on(event: "remove", listener: () => void): unknown;
  off(event: "remove", listener: () => void): unknown;
}): Promise<void> {
  let remaining = pool.totalCount;
  let removed = () => {};
  const disconnected = new Promise<void>((resolve) => {
    removed = () => { if (--remaining === 0) resolve(); };
    if (remaining === 0) resolve();
  });
  pool.on("remove", removed);
  try {
    await pool.end();
    await disconnected;
  } finally {
    pool.off("remove", removed);
  }
}

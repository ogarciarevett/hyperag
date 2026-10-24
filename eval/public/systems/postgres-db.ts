import type { Pool } from "pg";

// Local CI can opt into dev_* while published runs retain their pub_* names.
export function benchmarkDatabaseName(group: string, base: string, prefix = process.env.HYPERAG_DB_PREFIX ?? "pub_") {
  if (!/^[a-z][a-z0-9_]*_$/u.test(prefix)) throw new Error("invalid benchmark database prefix");
  return `${prefix}${group.replace(/\W/gu, "_")}_${base}`.toLowerCase().slice(0, 60);
}

export async function ensureBenchmarkDatabase(admin: Pick<Pool, "query">, db: string): Promise<void> {
  const ownership = () => admin.query<{ owned: boolean }>(
    "select pg_get_userbyid(datdba) = current_user as owned from pg_database where datname = $1", [db],
  );
  const found = await ownership();
  if (found.rowCount) {
    if (!found.rows[0]?.owned) throw new Error(`Benchmark database ${db} belongs to another role`);
    return;
  }
  try {
    await admin.query(`create database "${db.replaceAll('"', '""')}"`);
  } catch (error) {
    // Concurrent workers can both observe a missing database before either creates it.
    const code = (error as { code?: string }).code;
    if (code !== "23505" && code !== "42P04") throw error;
    if (!(await ownership()).rows[0]?.owned) throw error;
  }
}

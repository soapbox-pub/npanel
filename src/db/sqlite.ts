import {
  CompiledQuery,
  type DatabaseConnection,
  type Dialect,
  type Driver,
  type QueryCompiler,
  type QueryResult,
  SqliteAdapter,
  SqliteIntrospector,
  SqliteQueryCompiler,
  type TransactionSettings,
} from "kysely";
import type { DatabaseIntrospector, Kysely } from "kysely";

/**
 * The slice of a synchronous SQLite binding that this dialect needs.
 *
 * `node:sqlite` (Node 22.5+, Deno) and `bun:sqlite` both satisfy it, so npanel
 * runs on every target runtime with no native module to compile. Note that
 * `better-sqlite3` is deliberately avoided: its N-API usage crashes Bun.
 */
export interface SqliteDatabase {
  exec(sql: string): void;
  prepare(sql: string): SqliteStatement;
  close(): void;
}

export interface SqliteStatement {
  all(...params: SqliteValue[]): unknown[];
  run(...params: SqliteValue[]): {
    changes: number | bigint;
    lastInsertRowid: number | bigint;
  };
}

export type SqliteValue = string | number | bigint | null | Uint8Array;

/** Open a SQLite database using whichever synchronous binding the runtime has. */
export async function openSqlite(path: string): Promise<SqliteDatabase> {
  const db = await openDatabase(path);

  // First, because the statements below it can block: switching journal modes
  // takes a brief exclusive lock, and without a timeout already in force an
  // open that collides with one fails outright rather than waiting.
  db.exec("PRAGMA busy_timeout = 5000");

  // WAL keeps readers (blob serving) from blocking on the deploy thread's
  // writes, which come through a connection of its own.
  db.exec("PRAGMA journal_mode = WAL");
  db.exec("PRAGMA foreign_keys = ON");
  db.exec("PRAGMA synchronous = NORMAL");

  // Map the database into memory, so the point lookups on the serving path
  // read pages straight out of the OS page cache instead of paying a syscall
  // and a copy for each one. The number is a ceiling, not a reservation:
  // SQLite maps at most the file's actual size, and 256MB is far beyond any
  // plausible npanel database.
  db.exec("PRAGMA mmap_size = 268435456");

  return db;
}

async function openDatabase(path: string): Promise<SqliteDatabase> {
  try {
    const { DatabaseSync } = (await import("node:sqlite")) as {
      DatabaseSync: new (path: string) => SqliteDatabase;
    };
    return new DatabaseSync(path);
  } catch {
    // Not Node (or too old); fall through to Bun.
  }

  try {
    // Indirect specifier: TypeScript can't resolve `bun:sqlite` when the Node
    // types are in scope, and a static import would be a hard error there.
    const specifier = "bun:sqlite";
    const { Database } = (await import(specifier)) as {
      Database: new (path: string) => SqliteDatabase;
    };
    return new Database(path);
  } catch {
    // Neither binding is available.
  }

  throw new Error("No SQLite binding found. Requires Bun, or Node.js 22.5+ with node:sqlite.");
}

/** Kysely dialect over a {@link SqliteDatabase}. */
export class NPanelSqliteDialect implements Dialect {
  constructor(private readonly db: SqliteDatabase) {}

  createAdapter(): SqliteAdapter {
    return new SqliteAdapter();
  }

  createDriver(): Driver {
    return new SqliteDriver(this.db);
  }

  createIntrospector(db: Kysely<unknown>): DatabaseIntrospector {
    return new SqliteIntrospector(db);
  }

  createQueryCompiler(): QueryCompiler {
    return new SqliteQueryCompiler();
  }
}

/**
 * Single-connection driver. SQLite calls here are synchronous, so a promise
 * chain is all that's needed to keep transactions from interleaving.
 */
class SqliteDriver implements Driver {
  private connection: SqliteConnection;
  private queue: Promise<void> = Promise.resolve();

  constructor(db: SqliteDatabase) {
    this.connection = new SqliteConnection(db);
  }

  async init(): Promise<void> {}

  async acquireConnection(): Promise<DatabaseConnection> {
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const wait = this.queue;
    this.queue = wait.then(() => held);
    await wait;
    this.connection.release = release;
    return this.connection;
  }

  async beginTransaction(conn: DatabaseConnection, settings: TransactionSettings): Promise<void> {
    if (settings.isolationLevel && settings.isolationLevel !== "serializable") {
      throw new Error("SQLite only supports the serializable isolation level.");
    }
    await conn.executeQuery(CompiledQuery.raw("BEGIN"));
  }

  async commitTransaction(conn: DatabaseConnection): Promise<void> {
    await conn.executeQuery(CompiledQuery.raw("COMMIT"));
  }

  async rollbackTransaction(conn: DatabaseConnection): Promise<void> {
    await conn.executeQuery(CompiledQuery.raw("ROLLBACK"));
  }

  async releaseConnection(): Promise<void> {
    this.connection.release?.();
    this.connection.release = undefined;
  }

  async destroy(): Promise<void> {
    this.connection.close();
  }
}

/**
 * Prepared statements to keep, by SQL text.
 *
 * Kysely compiles the same query builder to the same string every time, so the
 * serving path's three lookups are three cache hits. Bounded because bulk
 * inserts are not like that: `storeManifest` chunks rows into a VALUES list
 * whose length the last chunk decides, and every length is a distinct string.
 */
const MAX_STATEMENTS = 256;

class SqliteConnection implements DatabaseConnection {
  release: (() => void) | undefined;
  private statements = new Map<string, SqliteStatement>();

  constructor(private readonly db: SqliteDatabase) {}

  /**
   * Prepare a statement, or reuse it if it's cached.
   *
   * Reuse is safe across migrations: both bindings sit on
   * `sqlite3_prepare_v2`, whose statements re-prepare themselves when the
   * schema changes rather than erroring.
   */
  private prepare(sql: string): SqliteStatement {
    const cached = this.statements.get(sql);
    if (cached) {
      // Re-insert on hit, so eviction below drops the least recently used.
      this.statements.delete(sql);
      this.statements.set(sql, cached);
      return cached;
    }

    const stmt = this.db.prepare(sql);

    if (this.statements.size >= MAX_STATEMENTS) {
      const oldest = this.statements.keys().next().value;
      if (oldest !== undefined) this.statements.delete(oldest);
    }
    this.statements.set(sql, stmt);

    return stmt;
  }

  async executeQuery<R>(compiled: CompiledQuery): Promise<QueryResult<R>> {
    const stmt = this.prepare(compiled.sql);
    const params = compiled.parameters.map(toSqliteValue);

    if (mutatesWithoutRows(compiled)) {
      const result = stmt.run(...params);
      return {
        rows: [],
        numAffectedRows: BigInt(result.changes),
        insertId: BigInt(result.lastInsertRowid),
      };
    }

    // node:sqlite returns null-prototype rows; spread them into plain objects
    // so downstream code (and JSON serialization) behaves predictably.
    return { rows: stmt.all(...params).map((row) => ({ ...(row as object) })) as R[] };
  }

  async *streamQuery<R>(compiled: CompiledQuery): AsyncIterableIterator<QueryResult<R>> {
    yield await this.executeQuery<R>(compiled);
  }

  close(): void {
    this.db.close();
  }
}

/** Whether a query returns no rows, so `run()` can report changes and insert ids. */
function mutatesWithoutRows(compiled: CompiledQuery): boolean {
  const node = compiled.query as { kind: string; returning?: unknown };
  const mutation =
    node.kind === "InsertQueryNode" ||
    node.kind === "UpdateQueryNode" ||
    node.kind === "DeleteQueryNode" ||
    node.kind === "MergeQueryNode";
  return mutation && node.returning === undefined;
}

function toSqliteValue(value: unknown): SqliteValue {
  if (value === null || value === undefined) return null;
  // node:sqlite refuses booleans outright; store them as SQLite's 0/1.
  if (typeof value === "boolean") return value ? 1 : 0;
  if (value instanceof Date) return Math.floor(value.getTime() / 1000);
  if (typeof value === "string" || typeof value === "number" || typeof value === "bigint") {
    return value;
  }
  if (value instanceof Uint8Array) return value;
  return JSON.stringify(value);
}

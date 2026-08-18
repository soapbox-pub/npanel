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

  // First, because everything below it can block: switching journal modes
  // needs a brief exclusive lock, and npanel opens this file from more than
  // one thread. Without a timeout already in force, a second connection
  // arriving at the wrong moment fails outright with "database is locked".
  db.exec("PRAGMA busy_timeout = 5000");

  // WAL keeps readers (blob serving) from blocking on the sync worker's writes.
  db.exec("PRAGMA journal_mode = WAL");
  db.exec("PRAGMA foreign_keys = ON");
  db.exec("PRAGMA synchronous = NORMAL");

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

class SqliteConnection implements DatabaseConnection {
  release: (() => void) | undefined;

  constructor(private readonly db: SqliteDatabase) {}

  async executeQuery<R>(compiled: CompiledQuery): Promise<QueryResult<R>> {
    const stmt = this.db.prepare(compiled.sql);
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

/**
 * RedDB JavaScript driver — TypeScript definitions.
 *
 * Hand-written, kept in sync with src/index.js.
 */

export interface ConnectOptions {
  /** Override the path to the `red` binary (defaults to bundled). */
  binary?: string
}

export interface QueryResult {
  statement: string
  affected: number
  columns: string[]
  rows: Array<Record<string, unknown>>
}

export interface InsertResult {
  affected: number
  /** Present when the underlying engine surfaces the inserted entity id. */
  id?: string | number
}

export interface BulkInsertResult {
  affected: number
}

export interface GetResult {
  entity: Record<string, unknown> | null
}

export interface DeleteResult {
  affected: number
}

export interface HealthResult {
  ok: boolean
  version: string
}

export interface VersionResult {
  version: string
  protocol: string
}

export class RedDBError extends Error {
  readonly name: 'RedDBError'
  readonly code: string
  readonly data: unknown
  constructor(code: string, message: string, data?: unknown)
}

export class RedDB {
  query(sql: string): Promise<QueryResult>
  insert(collection: string, payload: Record<string, unknown>): Promise<InsertResult>
  bulkInsert(
    collection: string,
    payloads: Array<Record<string, unknown>>,
  ): Promise<BulkInsertResult>
  get(collection: string, id: string | number): Promise<GetResult>
  delete(collection: string, id: string | number): Promise<DeleteResult>
  health(): Promise<HealthResult>
  version(): Promise<VersionResult>
  close(): Promise<void>
}

/**
 * Connect to a RedDB instance.
 *
 * Accepted URI schemes:
 *   - `memory://`              — ephemeral in-memory database
 *   - `file:///absolute/path`  — embedded, persisted to disk
 *   - `grpc://host:port`       — remote server (not yet supported)
 */
export function connect(uri: string, options?: ConnectOptions): Promise<RedDB>

/** Translate a connection URI into argv for `red rpc --stdio`. Exported for tests. */
export function uriToArgs(uri: string): string[]

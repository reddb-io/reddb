/**
 * @reddb-io/client — TypeScript definitions for the thin remote-only
 * RedDB driver. Hand-written, kept in sync with src/index.js.
 */

export type AuthOptions =
  | { token: string }
  | { apiKey: string }
  | { username: string; password: string; loginUrl?: string }

export interface TlsOptions {
  ca?: string | Uint8Array
  cert?: string | Uint8Array
  key?: string | Uint8Array
  servername?: string
  rejectUnauthorized?: boolean
}

export interface ConnectOptions {
  /** Authentication credentials for remote transports. */
  auth?: AuthOptions
  /** TLS options for `reds://` / `grpcs://` connections. */
  tls?: TlsOptions
}

export interface QueryResult {
  statement: string
  affected: number
  columns: string[]
  rows: Array<Record<string, unknown>>
}

export type QueryParam =
  | null
  | boolean
  | number
  | string
  | Uint8Array
  | Buffer
  | Date
  | Float32Array
  | Float64Array
  | number[]
  | Record<string, unknown>

export interface InsertResult { affected: number; rid: string | number; id: string | number }
export interface BulkInsertResult {
  affected: number
  rids: Array<string | number>
  ids: Array<string | number>
}
export interface GetResult { entity: Record<string, unknown> | null }
export interface DeleteResult { affected: number }
export interface CollectionMeta {
  name: string
  model: string
  capabilities: string[]
  [key: string]: unknown
}
export interface HealthResult { ok: boolean; version: string }
export interface VersionResult { version: string; protocol: string }

export type Role = 'read' | 'write' | 'admin'

export interface LoginResult {
  token: string
  username: string
  role: Role
  expires_at: number
}

export interface WhoamiResult { username: string; role: Role }
export interface CreateApiKeyResult { key: string; role: Role; created_at: number }
export interface ChangePasswordResult { ok: true }
export interface RevokeApiKeyResult { ok: true }

export class RedDBError extends Error {
  readonly name: 'RedDBError'
  readonly code: string
  readonly data: unknown
  constructor(code: string, message: string, data?: unknown)
}

// ---------------------------------------------------------------------------
// Cache API
// ---------------------------------------------------------------------------

export interface CachePutOptions {
  ttl_ms?: number
  tags?: string[]
  policy?: {
    idle_evict_ms?: number
    stale_while_revalidate_ms?: number
    jitter_ms?: number
  }
}

export type CacheExistsStatus = 'present' | 'absent' | 'maybe'

export class CacheClient {
  get(namespace: string, key: string): Promise<Uint8Array | null>
  put(
    namespace: string,
    key: string,
    value: Uint8Array | Buffer | string,
    opts?: CachePutOptions,
  ): Promise<void>
  exists(namespace: string, key: string): Promise<CacheExistsStatus>
  invalidate(namespace: string, key: string): Promise<void>
  invalidatePrefix(namespace: string, prefix: string): Promise<number>
  invalidateTags(namespace: string, tags: string[]): Promise<number>
  flushNamespace(namespace: string): Promise<void>
}

export interface KvWatchEvent {
  key: string
  op: 'insert' | 'update' | 'delete'
  before: unknown
  after: unknown
  lsn: number
  committed_at: number
  dropped_event_count: number
}

export class KvClient {
  put(
    key: string,
    value: unknown,
    options?: { collection?: string; expireMs?: number; tags?: string[] },
  ): Promise<QueryResult>
  get(key: string, options?: { collection?: string }): Promise<unknown | null>
  getMany(keys: string[], options?: { collection?: string }): Promise<Array<unknown | null>>
  exists(key: string, options?: { collection?: string }): Promise<{ exists: boolean }>
  delete(key: string, options?: { collection?: string }): Promise<DeleteResult>
  list(options?: { collection?: string; prefix?: string; limit?: number }): Promise<{
    items: Array<{ key: string; value: unknown }>
  }>
  invalidateTags(tags: string[], options?: { collection?: string }): Promise<number>
  watch(
    key: string,
    options?: { collection?: string; sinceLsn?: number; limit?: number },
  ): AsyncIterable<KvWatchEvent>
  watchPrefix(
    prefix: string,
    options?: { collection?: string; sinceLsn?: number; limit?: number },
  ): AsyncIterable<KvWatchEvent>
}

export interface DocumentInsertResult<T extends Record<string, unknown> = Record<string, unknown>> {
  affected: number
  rid: string | number
  item: T & { rid: string | number }
}

export class DocumentClient {
  insert<T extends Record<string, unknown> = Record<string, unknown>>(
    collection: string,
    document: Record<string, unknown>,
  ): Promise<DocumentInsertResult<T>>
  get<T extends Record<string, unknown> = Record<string, unknown>>(
    collection: string,
    rid: string | number,
  ): Promise<T & { rid: string | number }>
  list<T extends Record<string, unknown> = Record<string, unknown>>(
    collection: string,
    options?: { filter?: string; orderBy?: string; order_by?: string; limit?: number },
  ): Promise<{ items: Array<T & { rid: string | number }> }>
  patch<T extends Record<string, unknown> = Record<string, unknown>>(
    collection: string,
    rid: string | number,
    patch: Record<string, unknown>,
  ): Promise<T & { rid: string | number }>
  delete(collection: string, rid: string | number): Promise<DeleteResult>
}

export class QueueClient {
  push(
    queue: string,
    value: unknown,
    options?: { priority?: number },
  ): Promise<QueryResult>
  pop(queue: string, count?: number): Promise<unknown[]>
  peek(queue: string, count?: number): Promise<unknown[]>
  len(queue: string): Promise<number>
  purge(queue: string): Promise<QueryResult>
}

/**
 * Caller-typed SELECT builder. RedDB does not infer `T`; provide it
 * explicitly with `db.from<T>('collection')`.
 */
export class TypedQueryBuilder<T extends Record<string, unknown> = Record<string, unknown>> {
  select(): TypedQueryBuilder<T>
  select(column: '*'): TypedQueryBuilder<T>
  select<K extends keyof T & string>(...columns: K[]): TypedQueryBuilder<Pick<T, K>>
  select<K extends keyof T & string>(columns: K[]): TypedQueryBuilder<Pick<T, K>>
  where(condition: string, params: QueryParam[]): TypedQueryBuilder<T>
  where(condition: string, ...params: QueryParam[]): TypedQueryBuilder<T>
  run(): Promise<T[]>
}

export class ConfigClient {
  put(
    key: string,
    value: unknown,
    options?: {
      collection?: string
      tags?: string[]
      secretRef?: { collection: string; key: string }
    },
  ): Promise<QueryResult>
  get(key: string, options?: { collection?: string }): Promise<QueryResult>
  resolve(key: string, options?: { collection?: string }): Promise<QueryResult>
}

export class VaultClient {
  put(
    key: string,
    value: unknown,
    options?: { collection?: string; tags?: string[] },
  ): Promise<QueryResult>
  get(key: string, options?: { collection?: string }): Promise<QueryResult>
  unseal(key: string, options?: { collection?: string }): Promise<QueryResult>
}

/**
 * Specialised error thrown when an embedded URI is passed to the
 * thin client. Always has `code === 'EmbeddedNotSupported'`. Use
 * `@reddb-io/sdk` instead for in-memory or file-backed engines.
 */
export class EmbeddedNotSupported extends RedDBError {
  readonly name: 'EmbeddedNotSupported'
  readonly code: 'EmbeddedNotSupported'
  readonly uri: string
  constructor(uri: string)
}

export const EMBEDDED_REJECTION_MESSAGE: string

/** Returns true when `uri` selects the embedded engine. */
export function isEmbeddedUri(uri: string): boolean

export interface RedDBTransaction {
  query(sql: string): Promise<QueryResult>
  query(sql: string, params: QueryParam[]): Promise<QueryResult>
  query(sql: string, ...params: QueryParam[]): Promise<QueryResult>
  execute(sql: string): Promise<QueryResult>
  execute(sql: string, params: QueryParam[]): Promise<QueryResult>
  execute(sql: string, ...params: QueryParam[]): Promise<QueryResult>
  insert(collection: string, payload: Record<string, unknown>): Promise<InsertResult>
  bulkInsert(
    collection: string,
    payloads: Array<Record<string, unknown>>,
  ): Promise<BulkInsertResult>
  transaction<T>(
    callback: (tx: RedDBTransaction) => T | Promise<T>,
  ): Promise<T>
}

export class RedDB {
  readonly cache: CacheClient
  readonly queue: QueueClient
  readonly documents: DocumentClient
  readonly kv: KvClient & ((collection?: string) => KvClient)
  readonly config: (collection?: string) => ConfigClient
  readonly vault: (collection?: string) => VaultClient

  query(sql: string): Promise<QueryResult>
  query(sql: string, params: QueryParam[]): Promise<QueryResult>
  query(sql: string, ...params: QueryParam[]): Promise<QueryResult>
  execute(sql: string): Promise<QueryResult>
  execute(sql: string, params: QueryParam[]): Promise<QueryResult>
  execute(sql: string, ...params: QueryParam[]): Promise<QueryResult>
  insert(collection: string, payload: Record<string, unknown>): Promise<InsertResult>
  bulkInsert(
    collection: string,
    payloads: Array<Record<string, unknown>>,
  ): Promise<BulkInsertResult>
  transaction<T>(
    callback: (tx: RedDBTransaction) => T | Promise<T>,
  ): Promise<T>
  exists(collection: string): Promise<boolean>
  list(): Promise<CollectionMeta[]>
  /**
   * Caller-typed collection handle. Supply `T`; the SDK does not
   * generate or validate row types at runtime.
   */
  from<T extends Record<string, unknown> = Record<string, unknown>>(
    collection: string,
  ): TypedQueryBuilder<T>
  get(collection: string, id: string | number): Promise<GetResult>
  delete(collection: string, id: string | number): Promise<DeleteResult>
  health(): Promise<HealthResult>
  version(): Promise<VersionResult>

  login(username: string, password: string): Promise<LoginResult>
  whoami(): Promise<WhoamiResult>
  changePassword(currentPassword: string, newPassword: string): Promise<ChangePasswordResult>
  createApiKey(opts?: { username?: string; role?: Role }): Promise<CreateApiKeyResult>
  revokeApiKey(key: string): Promise<RevokeApiKeyResult>

  close(): Promise<void>
}

/**
 * Connect to a remote RedDB instance.
 *
 * Accepted URI schemes:
 *   - `red://host:port`        — RedWire TCP (default)
 *   - `reds://host:port`       — RedWire over TLS
 *   - `grpc://host:port`       — gRPC
 *   - `grpcs://host:port`      — gRPC over TLS
 *   - `http://host:port`       — HTTP JSON
 *   - `https://host:port`      — HTTPS JSON
 *
 * Embedded URIs (`memory://`, `memory:`, `file:///path`, `red:///`,
 * `red://:memory[:]`) throw `EmbeddedNotSupported`.
 */
export function connect(uri: string, options?: ConnectOptions): Promise<RedDB>

/** Exchange username + password for a bearer token via /auth/login. */
export function login(
  loginUrl: string,
  credentials: { username: string; password: string },
): Promise<LoginResult>

export interface ParsedUri {
  kind: 'embedded' | 'http' | 'https' | 'red' | 'reds' | 'grpc' | 'grpcs' | 'pg'
  host?: string
  port?: number
  path?: string
  username?: string
  password?: string
  token?: string
  apiKey?: string
  loginUrl?: string
  params?: URLSearchParams
  originalUri: string
}

export function parseUri(uri: string): ParsedUri
export function deriveLoginUrl(parsed: ParsedUri): string

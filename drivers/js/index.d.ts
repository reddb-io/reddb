/**
 * RedDB JavaScript driver — TypeScript definitions.
 *
 * Hand-written, kept in sync with src/index.js.
 */

export type AuthOptions =
  | { token: string }
  | { apiKey: string }
  | { username: string; password: string; loginUrl?: string }

/**
 * TLS / mTLS configuration for `redwire(s)://` connections.
 * `ca` / `cert` / `key` accept a filesystem path or a PEM string
 * starting with `-----BEGIN`.
 */
export interface TlsOptions {
  ca?: string | Uint8Array
  cert?: string | Uint8Array
  key?: string | Uint8Array
  servername?: string
  rejectUnauthorized?: boolean
}

export interface ConnectOptions {
  /** Override the path to the `red` binary (defaults to bundled). */
  binary?: string
  /** Rejected for embedded SDK connections; use @reddb-io/client remotely. */
  auth?: AuthOptions
  /** Reserved for remote clients; ignored by the embedded SDK. */
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

export interface AskSource {
  urn: string
  payload: string
}

export interface AskCitation {
  marker: number
  urn: string
}

export interface AskValidationItem {
  kind: string
  detail: string
}

export interface AskValidation {
  ok: boolean
  warnings: AskValidationItem[]
  errors: AskValidationItem[]
}

export interface AskQueryResult {
  answer: string
  cache_hit: boolean
  citations: AskCitation[]
  completion_tokens: number
  cost_usd: number
  mode: 'strict' | 'lenient'
  model: string
  prompt_tokens: number
  provider: string
  retry_count: number
  sources_flat: AskSource[]
  validation: AskValidation
}

export interface InsertResult {
  affected: number
  id: string | number
}

export interface BulkInsertResult {
  affected: number
  ids: Array<string | number>
}

export interface GetResult {
  entity: Record<string, unknown> | null
}

export interface DeleteResult {
  affected: number
}

export interface CollectionMeta {
  name: string
  model: string
  capabilities: string[]
  [key: string]: unknown
}

export interface HealthResult {
  ok: boolean
  version: string
}

export interface VersionResult {
  version: string
  protocol: string
}

export type Role = 'read' | 'write' | 'admin'

export interface LoginResult {
  token: string
  username: string
  role: Role
  expires_at: number
}

export interface WhoamiResult {
  username: string
  role: Role
}

export interface CreateApiKeyResult {
  key: string
  role: Role
  created_at: number
}

export interface ChangePasswordResult {
  ok: true
}

export interface RevokeApiKeyResult {
  ok: true
}

export class RedDBError extends Error {
  readonly name: 'RedDBError'
  readonly code: string
  readonly data: unknown
  constructor(code: string, message: string, data?: unknown)
}

// ---------------------------------------------------------------------------
// Cache API
// ---------------------------------------------------------------------------

/** Options for cache.put(). Maps to Rust BlobCachePolicy fields. */
export interface CachePutOptions {
  /** Entry TTL in milliseconds. Omit to use the namespace default. */
  ttl_ms?: number
  /** Tags for group invalidation via cache.invalidateTags(). */
  tags?: string[]
  /** Extended TTL policy (idle eviction, stale-while-revalidate, jitter). */
  policy?: {
    idle_evict_ms?: number
    stale_while_revalidate_ms?: number
    jitter_ms?: number
  }
}

export interface CacheGetResult {
  /** Raw bytes of the cached value. null when not found. */
  value: Uint8Array | null
}

export type CacheExistsStatus = 'present' | 'absent' | 'maybe'

export interface CacheInvalidateResult {
  removed: number
}

/**
 * Cache client. Requires an HTTP or gRPC / RedWire transport — the
 * underlying `cache.*` RPC methods are not served by the embedded
 * (stdio JSON-RPC) handler. Calls on an unsupported transport throw
 * `RedDBError` with code `UNSUPPORTED_TRANSPORT` before issuing any
 * RPC.
 */
export class CacheClient {
  /** Fetch a cached value. Returns Uint8Array on hit, null on miss. */
  get(namespace: string, key: string): Promise<Uint8Array | null>
  /** Store a value in the cache. */
  put(
    namespace: string,
    key: string,
    value: Uint8Array | Buffer | string,
    opts?: CachePutOptions,
  ): Promise<void>
  /** Check whether a key exists. */
  exists(namespace: string, key: string): Promise<CacheExistsStatus>
  /** Remove a single entry. */
  invalidate(namespace: string, key: string): Promise<void>
  /** Remove all entries whose key starts with prefix. Returns count removed. */
  invalidatePrefix(namespace: string, prefix: string): Promise<number>
  /** Remove all entries tagged with any of the given tags. Returns count removed. */
  invalidateTags(namespace: string, tags: string[]): Promise<number>
  /** Remove all entries in a namespace (routes to POST /admin/blob_cache/flush_namespace). */
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

export class RedDB {
  /** Underlying transport label. connect() returns 'embedded'. */
  readonly transport: string | null
  readonly cache: CacheClient
  readonly queue: QueueClient
  readonly kv: KvClient & ((collection?: string) => KvClient)
  readonly config: (collection?: string) => ConfigClient
  readonly vault: (collection?: string) => VaultClient

  query(sql: `ASK ${string}`): Promise<AskQueryResult>
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

  // Auth surface is not available through embedded SDK connections.
  // Use @reddb-io/client for remote authenticated servers.
  login(username: string, password: string): Promise<LoginResult>
  whoami(): Promise<WhoamiResult>
  changePassword(currentPassword: string, newPassword: string): Promise<ChangePasswordResult>
  createApiKey(opts?: { username?: string; role?: Role }): Promise<CreateApiKeyResult>
  revokeApiKey(key: string): Promise<RevokeApiKeyResult>

  close(): Promise<void>
}

/**
 * Connect to a RedDB instance.
 *
 * Accepted URI schemes:
 *   - `memory://`              — ephemeral in-memory database
 *   - `file:///absolute/path`  — embedded, persisted to disk
 *
 * Remote URI schemes throw `EMBEDDED_ONLY`; use @reddb-io/client.
 */
export function connect(uri: string, options?: ConnectOptions): Promise<RedDB>

export const EMBEDDED_ONLY_MESSAGE: string

/**
 * Translate a connection URI + (optional) auth into argv for
 * `red rpc --stdio`. Remote URI schemes throw `EMBEDDED_ONLY`.
 */
export function uriToArgs(
  uri: string,
  auth?: { kind: 'token'; token: string } | null,
): string[]

/**
 * Parsed `red://` (or legacy) URI. Returned by `parseUri`.
 */
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

/** Parse any URI (red://, memory://, file://, grpc://, http(s)://) into a normalised shape. */
export function parseUri(uri: string): ParsedUri

/** Derive the HTTP `/auth/login` URL from a parsed URI. */
export function deriveLoginUrl(parsed: ParsedUri): string

// ---------------------------------------------------------------
// RedWire native TCP transport (drivers/js/src/redwire.js)
// ---------------------------------------------------------------

/** RedWire frame kinds. Numeric values are the wire-stable spec. */
export const MessageKind: Readonly<{
  Query: 0x01
  Result: 0x02
  Error: 0x03
  BulkInsert: 0x04
  BulkOk: 0x05
  Hello: 0x10
  HelloAck: 0x11
  AuthRequest: 0x12
  AuthResponse: 0x13
  AuthOk: 0x14
  AuthFail: 0x15
  Bye: 0x16
  Ping: 0x17
  Pong: 0x18
}>

export type RedWireAuth =
  | { kind: 'anonymous' }
  | { kind: 'bearer'; token: string }

export interface RedWireConnectOptions {
  host: string
  port: number
  auth?: RedWireAuth
  clientName?: string
  /** When set, wraps the socket in TLS (mTLS via cert + key). */
  tls?: TlsOptions
}

/**
 * Open a RedWire connection. Speaks the binary protocol directly via
 * a TCP socket — no spawn, no fetch. Returned client matches the
 * `RpcClient` / `HttpRpcClient` surface so it slots into the
 * existing `RedDB` class.
 */
export function connectRedwire(opts: RedWireConnectOptions): Promise<RedWireClient>

export class RedWireClient {
  /**
   * Generic RPC entry. Routes:
   *  - 'query' → Query frame (SQL string)
   *  - 'insert' → BulkInsert frame, single-row shape
   *  - 'bulk_insert' → BulkInsert frame, array shape
   *  - 'health' / 'version' → Ping frame
   */
  call(method: string, params?: Record<string, unknown>): Promise<unknown>
  close(): Promise<void>
}

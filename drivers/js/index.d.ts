/**
 * RedDB JavaScript driver — TypeScript definitions.
 *
 * Hand-written, kept in sync with src/index.js.
 */

/**
 * Authentication credentials. Only meaningful for `grpc://` URIs;
 * embedded modes (memory://, file://) inherit the caller's
 * filesystem privileges and reject auth options at the boundary.
 *
 * The shape is `{ token }` (or its `{ apiKey }` alias). For
 * username/password login, mint a token first via the standalone
 * `login(httpUrl, { username, password })` helper, then pass it
 * here — the gRPC surface does not currently bridge `auth.login`.
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
  /** Authentication credentials (grpc:// only). */
  auth?: AuthOptions
  /**
   * TLS for `redwire(s)://` connections. URL params (`tls=true`,
   * `cert=`, `key=`, `ca=`) feed the same field; this option
   * always wins.
   */
  tls?: TlsOptions
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

  // Auth surface — only meaningful when connected via grpc://.
  // Embedded modes will receive 'unknown method' from the bridge.
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
 *   - `grpc://host:port`       — remote server
 */
export function connect(uri: string, options?: ConnectOptions): Promise<RedDB>

/**
 * Exchange username + password for a bearer token by hitting the
 * server's `POST /auth/login` HTTP endpoint. The returned `token`
 * can be passed to `connect(uri, { auth: { token } })`.
 *
 * @example
 * import { connect, login } from 'reddb'
 * const { token } = await login(
 *   'https://reddb.example.com/auth/login',
 *   { username: 'admin', password: 'secret' },
 * )
 * const db = await connect('grpc://reddb.example.com:5051', { auth: { token } })
 */
export function login(
  loginUrl: string,
  credentials: { username: string; password: string },
): Promise<LoginResult>

/**
 * Translate a connection URI + (optional) auth into argv for
 * `red rpc --stdio`. Exported for tests / debug. New code should
 * use `parseUri` directly and let `connect` handle dispatch.
 */
export function uriToArgs(
  uri: string,
  auth?: { kind: 'token'; token: string } | null,
): string[]

/**
 * Parsed `red://` (or legacy) URI. Returned by `parseUri`.
 */
export interface ParsedUri {
  kind: 'embedded' | 'http' | 'https' | 'grpc' | 'grpcs' | 'pg'
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

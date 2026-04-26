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

export interface ConnectOptions {
  /** Override the path to the `red` binary (defaults to bundled). */
  binary?: string
  /** Authentication credentials (grpc:// only). */
  auth?: AuthOptions
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
 * `red rpc --stdio`. Exported for tests; the second parameter is
 * the normalised auth shape from `connect`'s internal helper.
 */
export function uriToArgs(
  uri: string,
  auth?: { kind: 'token'; token: string } | null,
): string[]

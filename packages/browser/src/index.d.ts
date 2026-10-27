/** Local table SQL, transactions and persistence in browser-owned OPFS artifacts. */
export class BrowserError extends Error {
  readonly code: string
  constructor(code: string, message: string)
}

export interface RuntimeInfo {
  readonly abiVersion: 1
  readonly valueEncoding: 'reddb-json-v1'
  readonly capabilities: {
    readonly queryValidation: boolean
    readonly scalarQueries: boolean
    readonly storedQueries: boolean
    readonly storedQueryMode: 'read-write-tables'
    readonly sqlWrites: boolean
    readonly transactions: boolean
    readonly queryParameters?: boolean
    readonly databaseBackups?: boolean
    readonly sync: boolean
  }
}

export interface Runtime {
  /** Compiled engine capabilities; does not imply OPFS availability on this host. */
  info(): Promise<RuntimeInfo>
  /** Parse with the canonical RQL front-end; does not execute the statement. */
  validateQuery(sql: string): Promise<void>
  /** Storage-free scalar SELECT only; does not read or mutate the artifact. */
  executeScalar(sql: string): Promise<{ columns: string[]; rows: unknown[][] }>
  /** Terminate the storage-independent worker, rejecting pending operations. */
  close(): Promise<void>
}

export type InteropResult =
  | { ok: true; result: unknown }
  | { ok: false; error: { code: string; message: string } }

export interface RuntimeBridge {
  /** info, validateQuery or executeScalar. Failures resolve as structured envelopes. */
  invoke(method: string, params?: Record<string, unknown>): Promise<InteropResult>
  /** Synchronous, idempotent cancellation, including during initialization. */
  close(): void
}

/** No OPFS or Web Locks required; scalar execution only. */
export function createRuntime(options?: { worker?: Worker }): Promise<Runtime>
/** JSON-safe adapter boundary; does not expose artifact mutation or stored-data queries. */
export function createInteropRuntime(options?: { worker?: Worker }): RuntimeBridge

export interface Artifact extends Runtime {
  generation(): Promise<string>
  readWal(): Promise<Uint8Array[]>
  readSnapshot(): Promise<Uint8Array | undefined>
  /** Low-level native WAL payloads, not SQL commands. */
  append(payloads: Uint8Array[]): Promise<void>
  /** Complete native store snapshot; the caller must fold in the committed WAL. */
  checkpoint(snapshot: Uint8Array, minWalBytes?: number): Promise<void>
  /** Flush/close through the owner before terminating the worker. */
  close(): Promise<void>
}

/** Open/create an exclusively owned OPFS .rdb file. Another owner returns FILE_BUSY. */
export function openArtifact(options: {
  name: string
  /** Fresh module Worker supplied by a bundler (e.g. Vite ?worker). The artifact owns its lifetime. */
  worker?: Worker
}): Promise<Artifact>

/** Scalars bind as AST literals; integer numbers must be safe and bigints fit i64/u64. */
export type QueryParameter = null | boolean | number | string | bigint

export interface Database extends Runtime {
  /** SELECT and table SQL mutations; mutations return an affectedRows column. */
  query(sql: string, params?: readonly QueryParameter[]): Promise<{ columns: string[]; rows: unknown[][] }>
  /** Complete native .rdb image of committed state, at most 256 MiB. Pending SQL changes are excluded. */
  exportBackup(): Promise<Uint8Array>
  /** Release file ownership; standalone handles also terminate their Worker. */
  close(): Promise<void>
}

/** Recover an OPFS .rdb. Creation is explicit; corrupt existing files are preserved. */
export function openDatabase(options: {
  name: string
  /** Create a missing database. Defaults to false. */
  create?: boolean
  worker?: Worker
}): Promise<Database>

/** One initialized Worker, with at most one open database at a time. */
export interface DatabaseSession {
  /** The returned handle releases its file, but retains this session's Worker, on close. */
  openDatabase(options: { name: string; create?: boolean }): Promise<Database>
  /** Close the current file, discard uncommitted changes, and terminate the Worker. */
  close(): Promise<void>
}

/** Retain WASM initialization between file owners; callers must close the session when finished. */
export function createDatabaseSession(options?: { worker?: Worker }): Promise<DatabaseSession>

/** Restore a native .rdb image to a new name. Existing files are never overwritten. */
export function restoreDatabase(options: {
  name: string
  backup: Uint8Array
  worker?: Worker
}): Promise<Database>

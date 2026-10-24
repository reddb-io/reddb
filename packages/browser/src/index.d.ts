/** Persistence and scalar execution; stored-data queries and sync remain pending. */
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
    readonly transactions: boolean
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

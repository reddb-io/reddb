/** Experimental persistence and scalar execution; stored-data queries and sync remain pending. */
export class BrowserError extends Error {
  readonly code: string
  constructor(code: string, message: string)
}

export interface Artifact {
  /** Parse with the canonical RQL front-end; does not execute the statement. */
  validateQuery(sql: string): Promise<void>
  /** Storage-free scalar SELECT only; does not read or mutate the artifact. */
  executeScalar(sql: string): Promise<{ columns: string[]; rows: unknown[][] }>
  generation(): Promise<string>
  readWal(): Promise<Uint8Array[]>
  readSnapshot(): Promise<Uint8Array | undefined>
  /** Low-level native WAL payloads, not SQL commands. */
  append(payloads: Uint8Array[]): Promise<void>
  /** Complete native store snapshot; the caller must fold in the committed WAL. */
  checkpoint(snapshot: Uint8Array, minWalBytes?: number): Promise<void>
  close(): Promise<void>
}

/** Open/create an exclusively owned OPFS .rdb file. Another owner returns FILE_BUSY. */
export function openArtifact(options: { name: string }): Promise<Artifact>

/** Experimental persistence primitive; SQL execution and sync are not available. */
export class LiteError extends Error {
  readonly code: string
  constructor(code: string, message: string)
}

export interface Artifact {
  /** Parse with the canonical RQL front-end; does not execute the statement. */
  validateQuery(sql: string): Promise<void>
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

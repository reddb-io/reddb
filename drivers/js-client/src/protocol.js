/**
 * JSON-RPC 2.0 line-delimited client.
 *
 * Maintains a map of pending requests keyed by id. Reads stdout one
 * line at a time, parses each line as a response envelope, looks up
 * the pending promise by id and resolves/rejects it.
 *
 * Spec: PLAN_DRIVERS.md, "Spec do protocolo stdio".
 */

const NEWLINE = 0x0a // '\n'
const encoder = new TextEncoder()
const decoder = new TextDecoder('utf-8')

/**
 * RedDB-shaped error. Drivers in other languages should expose an
 * equivalent class with the same `code` field.
 */
export class RedDBError extends Error {
  constructor(code, message, data) {
    super(message)
    this.name = 'RedDBError'
    this.code = code
    this.data = data ?? null
  }
}

export class RpcClient {
  /** @param {import('./spawn.js').RedProcess} child */
  constructor(child) {
    this.child = child
    this.nextId = 1
    /** @type {Map<number|string, { resolve: Function, reject: Function }>} */
    this.pending = new Map()
    this.closed = false
    this.closeReason = null
    this.readerPromise = this.#readLoop()
  }

  /**
   * Send a JSON-RPC 2.0 request and resolve with the result, or reject
   * with a `RedDBError` if the server returned an error envelope.
   */
  call(method, params = {}) {
    if (this.closed) {
      return Promise.reject(
        new RedDBError('CLIENT_CLOSED', `client is closed: ${this.closeReason ?? 'unknown'}`),
      )
    }
    const id = this.nextId++
    const envelope = JSON.stringify({ jsonrpc: '2.0', id, method, params })
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject })
      this.child.stdin.write(encoder.encode(envelope + '\n')).catch((err) => {
        this.pending.delete(id)
        reject(err)
      })
    })
  }

  /** Drain pending requests, send `close`, wait for the binary to exit. */
  async close() {
    if (this.closed) return
    try {
      await this.call('close', {})
    } catch {
      // best effort — server may already be gone
    }
    this.#shutdown('close requested')
    try {
      this.child.stdin.end()
    } catch {
      // ignore
    }
    await this.child.wait()
  }

  // -------------------------------------------------------------------------
  // Internal: stdout reader loop
  // -------------------------------------------------------------------------

  async #readLoop() {
    let buffer = new Uint8Array(0)
    try {
      for await (const chunk of this.child.stdout) {
        const merged = new Uint8Array(buffer.length + chunk.length)
        merged.set(buffer, 0)
        merged.set(chunk, buffer.length)
        buffer = merged

        // Split on \n, dispatch each complete line.
        let start = 0
        for (let i = 0; i < buffer.length; i++) {
          if (buffer[i] === NEWLINE) {
            const lineBytes = buffer.subarray(start, i)
            this.#dispatchLine(decoder.decode(lineBytes))
            start = i + 1
          }
        }
        if (start > 0) {
          buffer = buffer.subarray(start)
        }
      }
    } catch (err) {
      this.#shutdown(`stdout reader error: ${err.message}`)
      return
    }
    // EOF — server exited.
    this.#shutdown('server stdout closed')
  }

  #dispatchLine(line) {
    if (!line.trim()) return
    let envelope
    try {
      envelope = JSON.parse(line)
    } catch (err) {
      // Malformed line from the server is fatal to the protocol —
      // we cannot map it to any pending request.
      this.#shutdown(`malformed server response: ${err.message}`)
      return
    }
    const id = envelope.id
    if (id === null || id === undefined) {
      // No id → cannot route. Treat as protocol violation.
      return
    }
    const pending = this.pending.get(id)
    if (!pending) {
      // Unknown id — server bug or duplicate response. Ignore.
      return
    }
    this.pending.delete(id)
    if (envelope.error) {
      pending.reject(
        new RedDBError(
          envelope.error.code ?? 'UNKNOWN',
          envelope.error.message ?? 'unknown error',
          envelope.error.data,
        ),
      )
    } else {
      pending.resolve(envelope.result)
    }
  }

  #shutdown(reason) {
    if (this.closed) return
    this.closed = true
    this.closeReason = reason
    const err = new RedDBError('CLIENT_CLOSED', reason)
    for (const { reject } of this.pending.values()) {
      reject(err)
    }
    this.pending.clear()
  }
}

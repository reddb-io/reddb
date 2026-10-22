/** Experimental artifact storage. This API does not execute queries or sync. */

export class LiteError extends Error {
  constructor(code, message) {
    super(message)
    this.name = 'LiteError'
    this.code = code
  }
}

/** Open an exclusively owned, canonical .rdb artifact in an OPFS worker. */
export async function openArtifact({ name } = {}) {
  if (typeof name !== 'string' || !/^[a-zA-Z0-9_-][a-zA-Z0-9_.-]{0,79}$/.test(name)) {
    throw new LiteError('INVALID_NAME', 'Artifact names must contain 1–80 letters, digits, dots, underscores or hyphens')
  }
  if (!globalThis.isSecureContext || !globalThis.Worker || !globalThis.navigator?.storage?.getDirectory || !globalThis.navigator?.locks) {
    throw new LiteError('STORAGE_UNSUPPORTED', 'RedDB Lite requires HTTPS, module Workers, OPFS and Web Locks')
  }

  const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' })
  const pending = new Map()
  let sequence = 0
  let closed = false

  function fail(error) {
    closed = true
    for (const request of pending.values()) request.reject(error)
    pending.clear()
    worker.terminate()
  }

  worker.onmessage = ({ data }) => {
    const request = pending.get(data.id)
    if (!request) return
    pending.delete(data.id)
    if (data.error) request.reject(new LiteError(data.error.code, data.error.message))
    else request.resolve(data.result)
  }
  worker.onerror = (event) => fail(new LiteError('WORKER_FAILED', event.message || 'RedDB storage worker failed'))
  worker.onmessageerror = () => fail(new LiteError('WORKER_FAILED', 'Unable to decode a storage worker response'))

  function call(method, params = {}) {
    if (closed) return Promise.reject(new LiteError('CLOSED', 'This artifact handle is closed'))
    const id = ++sequence
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject })
      try { worker.postMessage({ id, method, params }) }
      catch (error) { pending.delete(id); reject(error) }
    })
  }

  try { await call('open', { name }) }
  catch (error) { fail(error); throw error }

  let closing
  return {
    validateQuery: (sql) => call('validateQuery', { sql }),
    generation: () => call('generation'),
    readWal: () => call('readWal'),
    readSnapshot: () => call('readSnapshot'),
    append: (payloads) => call('append', { payloads }),
    checkpoint: (snapshot, minWalBytes = 0) => call('checkpoint', { snapshot, minWalBytes }),
    close() {
      if (!closing) {
        closing = closed ? Promise.resolve() : call('close').finally(() => {
          closed = true
          worker.terminate()
        })
        // Reject calls issued after close(), including calls in the same turn.
        closed = true
      }
      return closing
    },
  }
}

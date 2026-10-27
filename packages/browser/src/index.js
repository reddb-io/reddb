/** Shared Rust/WASM runtime and artifact storage. */
import { BrowserError, RUNTIME_ABI_VERSION, VALUE_ENCODING, MAX_QUERY_PARAMETERS, MAX_DATABASE_BACKUP_BYTES } from './protocol.js'
export { BrowserError } from './protocol.js'

function createClient(suppliedWorker) {
  if (!globalThis.Worker) throw new BrowserError('RUNTIME_UNSUPPORTED', 'RedDB requires module Workers and WebAssembly')
  if (suppliedWorker !== undefined && !(suppliedWorker instanceof Worker)) {
    throw new BrowserError('INVALID_ARGUMENT', 'worker must be a dedicated Worker instance')
  }
  const worker = suppliedWorker ?? new Worker(new URL('./worker.js', import.meta.url), { type: 'module' })
  const pending = new Map()
  let sequence = 0
  let closed = false
  let terminated = false

  function shutdown(error = new BrowserError('CLOSED', 'This runtime handle is closed')) {
    closed = true
    for (const request of pending.values()) request.reject(error)
    pending.clear()
    if (!terminated) worker.terminate()
    terminated = true
  }

  worker.onmessage = ({ data }) => {
    if (terminated) return
    if (data?.abiVersion !== RUNTIME_ABI_VERSION) {
      shutdown(new BrowserError('VERSION_MISMATCH', 'Worker runtime ABI does not match this SDK'))
      return
    }
    if (!Number.isSafeInteger(data.id) || data.id < 1) {
      shutdown(new BrowserError('WORKER_FAILED', 'Invalid runtime response identifier'))
      return
    }
    const request = pending.get(data.id)
    if (!request) return
    pending.delete(data.id)
    if (data.error) request.reject(new BrowserError(data.error.code, data.error.message))
    else request.resolve(data.result)
  }
  worker.onerror = (event) => shutdown(new BrowserError('WORKER_FAILED', event.message || 'RedDB worker failed'))
  worker.onmessageerror = () => shutdown(new BrowserError('WORKER_FAILED', 'Unable to decode a worker response'))

  function call(method, params = {}) {
    if (closed) return Promise.reject(new BrowserError('CLOSED', 'This runtime handle is closed'))
    const id = ++sequence
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject })
      try { worker.postMessage({ abiVersion: RUNTIME_ABI_VERSION, id, method, params }) }
      catch (error) { pending.delete(id); reject(error) }
    })
  }

  const ready = call('initialize').then(info => {
    if (info?.abiVersion !== RUNTIME_ABI_VERSION || info.valueEncoding !== VALUE_ENCODING
      || info.capabilities?.queryValidation !== true || info.capabilities?.scalarQueries !== true) {
      throw new BrowserError('VERSION_MISMATCH', 'WASM runtime capabilities do not match this SDK')
    }
    return info
  }).catch(error => { shutdown(error); throw error })
  ready.catch(() => {}) // A bridge can be closed before its first invocation.
  let closing
  return {
    ready, call, shutdown,
    closeFile() {
      if (!closing) {
        closing = closed ? Promise.resolve() : call('close').finally(() => shutdown())
        closed = true
      }
      return closing
    },
  }
}

function runtimeMethods(client) {
  const query = (method, sql) => typeof sql === 'string'
    ? client.call(method, { sql })
    : Promise.reject(new BrowserError('INVALID_ARGUMENT', 'Expected an RQL string'))
  return {
    info: () => client.call('info'),
    validateQuery: sql => query('validateQuery', sql),
    executeScalar: sql => query('executeScalar', sql),
  }
}

/** Storage-independent Rust/WASM operations; does not open an OPFS file. */
export async function createRuntime({ worker } = {}) {
  const client = createClient(worker)
  await client.ready
  return { ...runtimeMethods(client), close() { client.shutdown(); return Promise.resolve() } }
}

/** JSON-safe boundary for Dart, .NET and other JS-interop hosts. */
export function createInteropRuntime({ worker } = {}) {
  let client
  let ready
  let closed = false
  try {
    client = createClient(worker)
    ready = client.ready.then(() => runtimeMethods(client))
  } catch (error) { ready = Promise.reject(error) }
  ready.catch(() => {})
  return {
    async invoke(method, params = {}) {
      try {
        if (closed) throw new BrowserError('CLOSED', 'This runtime handle is closed')
        const runtime = await ready
        if (!params || typeof params !== 'object' || Array.isArray(params)) {
          throw new BrowserError('INVALID_ARGUMENT', 'Expected an argument object')
        }
        let result
        switch (method) {
          case 'info': result = await runtime.info(); break
          case 'validateQuery': result = await runtime.validateQuery(params.sql); break
          case 'executeScalar': result = await runtime.executeScalar(params.sql); break
          default: throw new BrowserError('INVALID_ARGUMENT', 'Unknown runtime operation')
        }
        return { ok: true, result: result ?? null }
      } catch (error) {
        return { ok: false, error: {
          code: typeof error.code === 'string' ? error.code : 'RUNTIME_ERROR',
          message: error.message || String(error),
        } }
      }
    },
    close() { closed = true; client?.shutdown() },
  }
}

async function openFile({ name, worker, create = false, backup } = {}, method = 'open') {
  if (typeof create !== 'boolean') throw new BrowserError('INVALID_ARGUMENT', 'Expected a boolean create option')
  if (typeof name !== 'string' || !/^[a-zA-Z0-9_-][a-zA-Z0-9_.-]{0,79}$/.test(name)) {
    throw new BrowserError('INVALID_NAME', 'Artifact names must contain 1–80 letters, digits, dots, underscores or hyphens')
  }
  if (!globalThis.isSecureContext || !globalThis.Worker || !globalThis.navigator?.storage?.getDirectory || !globalThis.navigator?.locks) {
    throw new BrowserError('STORAGE_UNSUPPORTED', 'RedDB Browser requires HTTPS, module Workers, OPFS and Web Locks')
  }
  const client = createClient(worker)
  try {
    const info = await client.ready
    const sqlDatabase = method === 'openDatabase' || method === 'restoreDatabase'
    if (sqlDatabase && (info.capabilities.storedQueries !== true
      || info.capabilities.storedQueryMode !== 'read-write-tables' || !info.capabilities.sqlWrites || !info.capabilities.transactions
      || info.capabilities.queryParameters !== true)) {
      throw new BrowserError('VERSION_MISMATCH', 'WASM runtime does not expose local table SQL, transactions and parameters')
    }
    if (method === 'restoreDatabase' && info.capabilities.databaseBackups !== true) {
      throw new BrowserError('VERSION_MISMATCH', 'Worker runtime does not expose database backups')
    }
    await client.call(method, sqlDatabase ? { name, create, ...(backup === undefined ? {} : { backup }) } : { name })
  }
  catch (error) { client.shutdown(error); throw error }
  return client
}

/** Open an exclusively owned, canonical .rdb artifact in an OPFS worker. */
export async function openArtifact(options) {
  const client = await openFile(options)
  const { call } = client
  return {
    ...runtimeMethods(client),
    generation: () => call('generation'),
    readWal: () => call('readWal'),
    readSnapshot: () => call('readSnapshot'),
    append: (payloads) => call('append', { payloads }),
    checkpoint: (snapshot, minWalBytes = 0) => call('checkpoint', { snapshot, minWalBytes }),
    close: client.closeFile,
  }
}

/** Local table SQL and single-owner transactions over a canonical OPFS .rdb. */
export async function openDatabase(options) {
  return databaseMethods(await openFile(options, 'openDatabase'))
}

/** Restore a native .rdb backup to a new, exclusively owned OPFS filename. */
export async function restoreDatabase(options) {
  const backup = options?.backup
  if (!(backup instanceof Uint8Array) || !backup.byteLength || backup.byteLength > MAX_DATABASE_BACKUP_BYTES) {
    throw new BrowserError('INVALID_ARGUMENT', 'Expected a nonempty .rdb backup of at most 256 MiB as Uint8Array')
  }
  return databaseMethods(await openFile(options, 'restoreDatabase'))
}

function databaseMethods(client) {
  return {
    ...runtimeMethods(client),
    query(sql, params) {
      if (typeof sql !== 'string') return Promise.reject(new BrowserError('INVALID_ARGUMENT', 'Expected an RQL string'))
      if (params === undefined) return client.call('query', { sql })
      if (!Array.isArray(params) || params.length > MAX_QUERY_PARAMETERS) {
        return Promise.reject(new BrowserError('INVALID_ARGUMENT', 'Expected an array of at most 65536 positional parameters'))
      }
      return client.call('queryWithParams', { sql, params })
    },
    exportBackup: () => client.call('exportBackup'),
    close: client.closeFile,
  }
}

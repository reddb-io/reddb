import init, * as wasm from './wasm/reddb_wasm.js'
import { RUNTIME_ABI_VERSION, VALUE_ENCODING, MAX_QUERY_PARAMETERS, MAX_DATABASE_BACKUP_BYTES } from './protocol.js'

let artifact
let database
let access
let release
let ownership
let queue = Promise.resolve()
let initialized

function initialize() {
  return initialized ??= (async () => {
    await init()
    if (typeof wasm.runtimeInfo !== 'function') throw error('VERSION_MISMATCH', 'WASM runtime metadata is missing')
    let info
    try { info = wasm.runtimeInfo() }
    catch { throw error('VERSION_MISMATCH', 'WASM binary does not expose the expected runtime ABI') }
    if (info.abiVersion !== RUNTIME_ABI_VERSION || info.valueEncoding !== VALUE_ENCODING) {
      throw error('VERSION_MISMATCH', 'WASM runtime ABI does not match this worker')
    }
    return info
  })()
}

function error(code, message) {
  return Object.assign(new Error(message), { code })
}

function synchronous(value) {
  if (value && typeof value.then === 'function') {
    value.catch(() => {})
    throw error('STORAGE_UNSUPPORTED', 'This browser implements asynchronous OPFS access; synchronous access is required')
  }
  return value
}

async function open(name, sqlDatabase = false, create = false, backup) {
  const restoring = backup !== undefined
  await initialize()
  if (artifact || database) throw error('INVALID_ARGUMENT', 'This worker already owns an open file')
  await new Promise((resolve, reject) => {
    ownership = navigator.locks.request(`reddb-browser:${name}`, { ifAvailable: true }, async (lock) => {
      if (!lock) throw error('FILE_BUSY', 'This artifact is already open in another worker or tab')
      let directory
      let created = false
      let opened = false
      try {
        const root = await navigator.storage.getDirectory()
        directory = await root.getDirectoryHandle('reddb-browser', { create: !sqlDatabase || create || restoring })
        let file
        try {
          file = await directory.getFileHandle(`${name}.rdb`, { create: !sqlDatabase })
          if (restoring) throw error('DATABASE_EXISTS', 'Restore requires a new database name; existing files are never overwritten')
        }
        catch (failure) {
          if (!sqlDatabase || (!create && !restoring) || failure.name !== 'NotFoundError') throw failure
          file = await directory.getFileHandle(`${name}.rdb`, { create: true })
          created = true
        }
        if (typeof file.createSyncAccessHandle !== 'function') {
          throw error('STORAGE_UNSUPPORTED', 'This browser does not support synchronous OPFS access in a dedicated worker')
        }
        access = await file.createSyncAccessHandle()
        if (restoring) {
          let offset = 0
          while (offset < backup.byteLength) {
            const written = synchronous(access.write(backup.subarray(offset, offset + 1024 * 1024), { at: offset }))
            if (!written) throw error('ARTIFACT_ERROR', 'Unable to write the complete database backup')
            offset += written
          }
          synchronous(access.flush())
          backup = undefined
        }
        const fileIo = {
          readAt: (buffer, offset) => synchronous(access.read(buffer, { at: offset })),
          writeAt: (buffer, offset) => synchronous(access.write(buffer, { at: offset })),
          getSize: () => synchronous(access.getSize()),
          truncate: (size) => synchronous(access.truncate(size)),
          flush: () => synchronous(access.flush()),
        }
        if (sqlDatabase) database = created && !restoring
          ? wasm.BrowserDatabase.create(fileIo, name, Date.now())
          : wasm.BrowserDatabase.open(fileIo, name)
        else artifact = fileIo.getSize() === 0
            ? wasm.BrowserArtifact.create(fileIo, name, Date.now())
            : wasm.BrowserArtifact.open(fileIo, name)
        opened = true
        const lifetime = new Promise((resolveRelease) => { release = resolveRelease })
        resolve()
        await lifetime
      } catch (failure) {
        const reported = sqlDatabase && failure.name === 'NotFoundError'
          ? error('DATABASE_NOT_FOUND', 'The local .rdb file does not exist') : failure
        throw reported
      } finally {
        artifact?.free()
        artifact = undefined
        database?.free()
        database = undefined
        if (access) synchronous(access.close())
        access = undefined
        // Reject through ownership.catch only after cleanup, before the SDK can
        // terminate this Worker. An interrupted restore may leave a new file;
        // existing destinations are always preserved.
        if (restoring && created && !opened) await directory.removeEntry(`${name}.rdb`)
      }
    })
    // Observe early lock/open failures, including ifAvailable returning null.
    ownership.catch(reject)
  })
}

function bytes(value) {
  if (!(value instanceof Uint8Array)) throw error('INVALID_ARGUMENT', 'Expected a Uint8Array')
  return value
}

function backupBytes(value) {
  const backup = bytes(value)
  if (!backup.byteLength || backup.byteLength > MAX_DATABASE_BACKUP_BYTES) {
    throw error('INVALID_ARGUMENT', 'Expected a nonempty .rdb backup of at most 256 MiB')
  }
  return backup
}

function exportBackup() {
  database.checkHealthy()
  const size = synchronous(access.getSize())
  if (!Number.isSafeInteger(size) || size < 1 || size > MAX_DATABASE_BACKUP_BYTES) {
    throw error('BACKUP_SIZE_LIMIT', 'Database backups must fit within 256 MiB')
  }
  synchronous(access.flush())
  const backup = new Uint8Array(size)
  let offset = 0
  while (offset < size) {
    const read = synchronous(access.read(backup.subarray(offset, offset + 1024 * 1024), { at: offset }))
    if (!read) throw error('ARTIFACT_ERROR', 'Unable to read the complete database backup')
    offset += read
  }
  return backup
}

async function execute(method, params) {
  if (method === 'initialize' || method === 'info') return initialize()
  if (method === 'open') return open(params.name)
  if (method === 'restoreDatabase') return open(params.name, true, false, backupBytes(params.backup))
  if (method === 'openDatabase') {
    if (typeof params.create !== 'boolean') throw error('INVALID_ARGUMENT', 'Expected a boolean create option')
    return open(params.name, true, params.create)
  }
  if (method === 'validateQuery' || method === 'executeScalar') {
    await initialize()
    if (typeof params.sql !== 'string') throw error('INVALID_ARGUMENT', 'Expected an RQL string')
    return method === 'validateQuery' ? wasm.validateQuery(params.sql) : wasm.executeScalar(params.sql)
  }
  if (method === 'close') {
    if (artifact || database) {
      release()
      await ownership
    }
    return
  }
  if (!artifact && !database) throw error('CLOSED', 'The file is not open')
  if (database) {
    if (method === 'catalog') {
      if (typeof database.catalog !== 'function') throw error('VERSION_MISMATCH', 'WASM catalog inspection is missing')
      return database.catalog()
    }
    if (method === 'readQuery') {
      if (typeof params.sql !== 'string' || !Array.isArray(params.params) || params.params.length > MAX_QUERY_PARAMETERS || !Array.isArray(params.scopes) || params.scopes.length > 64) throw error('INVALID_ARGUMENT', 'Expected SQL, parameters and at most 64 inspection scopes')
      if (typeof database.readQuery !== 'function') throw error('VERSION_MISMATCH', 'WASM scoped reads are missing')
      return database.readQuery(params.sql, params.params, params.scopes)
    }
    if (method === 'exportBackup') {
      try { return exportBackup() }
      catch (failure) {
        if (failure.code !== 'DATABASE_ERROR' && failure.code !== 'BACKUP_SIZE_LIMIT') {
          release()
          await ownership
        }
        throw failure
      }
    }
    if (method !== 'query' && method !== 'queryWithParams') throw error('QUERY_UNSUPPORTED', 'This database handle exposes local SQL')
    if (typeof params.sql !== 'string') throw error('INVALID_ARGUMENT', 'Expected an RQL string')
    if (method === 'queryWithParams') {
      if (!Array.isArray(params.params) || params.params.length > MAX_QUERY_PARAMETERS) {
        throw error('INVALID_ARGUMENT', 'Expected an array of at most 65536 positional parameters')
      }
      if (typeof database.queryWithParams !== 'function') throw error('VERSION_MISMATCH', 'WASM parameter binding is missing')
      return database.queryWithParams(params.sql, params.params, Date.now())
    }
    return database.query(params.sql, Date.now())
  }
  switch (method) {
    case 'generation': return artifact.generation()
    case 'readWal': return artifact.readWal()
    case 'readSnapshot': return artifact.readSnapshot()
    case 'append':
      if (!Array.isArray(params.payloads)) throw error('INVALID_ARGUMENT', 'Expected an array of WAL payloads')
      return artifact.append(params.payloads.map(bytes))
    case 'checkpoint':
      if (!Number.isInteger(params.minWalBytes) || params.minWalBytes < 0 || params.minWalBytes > 0xffffffff) {
        throw error('INVALID_ARGUMENT', 'WAL capacity must be an unsigned 32-bit integer')
      }
      return artifact.checkpoint(bytes(params.snapshot), params.minWalBytes)
    default: throw error('INVALID_ARGUMENT', `Unknown artifact operation: ${method}`)
  }
}

self.onmessage = ({ data }) => {
  const { id, method, params = {} } = data ?? {}
  if (data?.abiVersion !== RUNTIME_ABI_VERSION) {
    self.postMessage({ abiVersion: RUNTIME_ABI_VERSION, id, error: { code: 'VERSION_MISMATCH', message: 'SDK runtime ABI does not match this worker' } })
    return
  }
  // Opening is asynchronous; all following operations wait for ownership.
  queue = queue.then(async () => {
    try {
      const result = await execute(method, params)
      self.postMessage({ abiVersion: RUNTIME_ABI_VERSION, id, result }, method === 'exportBackup' ? [result.buffer] : [])
    } catch (failure) {
      self.postMessage({ abiVersion: RUNTIME_ABI_VERSION, id, error: { code: failure.code || 'ARTIFACT_ERROR', message: failure.message || String(failure) } })
    }
  })
}

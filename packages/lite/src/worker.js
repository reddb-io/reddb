import init, { BrowserArtifact, validateQuery } from './wasm/reddb_wasm.js'

let artifact
let access
let release
let ownership
let queue = Promise.resolve()

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

async function open(name) {
  await init()
  await new Promise((resolve, reject) => {
    ownership = navigator.locks.request(`reddb-lite:${name}`, { ifAvailable: true }, async (lock) => {
      if (!lock) throw error('FILE_BUSY', 'This artifact is already open in another worker or tab')
      try {
        const root = await navigator.storage.getDirectory()
        const directory = await root.getDirectoryHandle('reddb-lite', { create: true })
        const file = await directory.getFileHandle(`${name}.rdb`, { create: true })
        if (typeof file.createSyncAccessHandle !== 'function') {
          throw error('STORAGE_UNSUPPORTED', 'This browser does not support synchronous OPFS access in a dedicated worker')
        }
        access = await file.createSyncAccessHandle()
        const fileIo = {
          readAt: (buffer, offset) => synchronous(access.read(buffer, { at: offset })),
          writeAt: (buffer, offset) => synchronous(access.write(buffer, { at: offset })),
          getSize: () => synchronous(access.getSize()),
          truncate: (size) => synchronous(access.truncate(size)),
          flush: () => synchronous(access.flush()),
        }
        artifact = fileIo.getSize() === 0
          ? BrowserArtifact.create(fileIo, name, Date.now())
          : BrowserArtifact.open(fileIo, name)
        const lifetime = new Promise((resolveRelease) => { release = resolveRelease })
        resolve()
        await lifetime
      } catch (failure) {
        reject(failure)
        throw failure
      } finally {
        artifact?.free()
        artifact = undefined
        if (access) synchronous(access.close())
        access = undefined
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

async function execute(method, params) {
  if (method === 'open') return open(params.name)
  if (!artifact) throw error('CLOSED', 'The artifact is not open')
  switch (method) {
    case 'validateQuery':
      if (typeof params.sql !== 'string') throw error('INVALID_ARGUMENT', 'Expected an RQL string')
      return validateQuery(params.sql)
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
    case 'close':
      release()
      await ownership
      return
    default: throw error('INVALID_ARGUMENT', `Unknown artifact operation: ${method}`)
  }
}

self.onmessage = ({ data: { id, method, params } }) => {
  // Opening is asynchronous; all following operations wait for ownership.
  queue = queue.then(async () => {
    try {
      const result = await execute(method, params)
      self.postMessage({ id, result })
    } catch (failure) {
      self.postMessage({ id, error: { code: failure.code || 'ARTIFACT_ERROR', message: failure.message || String(failure) } })
    }
  })
}

import init, * as wasm from './wasm/reddb_wasm.js'
import { RUNTIME_ABI_VERSION, VALUE_ENCODING } from './protocol.js'

let artifact
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

async function open(name) {
  await initialize()
  await new Promise((resolve, reject) => {
    ownership = navigator.locks.request(`reddb-browser:${name}`, { ifAvailable: true }, async (lock) => {
      if (!lock) throw error('FILE_BUSY', 'This artifact is already open in another worker or tab')
      try {
        const root = await navigator.storage.getDirectory()
        const directory = await root.getDirectoryHandle('reddb-browser', { create: true })
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
          ? wasm.BrowserArtifact.create(fileIo, name, Date.now())
          : wasm.BrowserArtifact.open(fileIo, name)
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
  if (method === 'initialize' || method === 'info') return initialize()
  if (method === 'open') return open(params.name)
  if (method === 'validateQuery' || method === 'executeScalar') {
    await initialize()
    if (typeof params.sql !== 'string') throw error('INVALID_ARGUMENT', 'Expected an RQL string')
    return method === 'validateQuery' ? wasm.validateQuery(params.sql) : wasm.executeScalar(params.sql)
  }
  if (!artifact) throw error('CLOSED', 'The artifact is not open')
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
    case 'close':
      release()
      await ownership
      return
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
      self.postMessage({ abiVersion: RUNTIME_ABI_VERSION, id, result })
    } catch (failure) {
      self.postMessage({ abiVersion: RUNTIME_ABI_VERSION, id, error: { code: failure.code || 'ARTIFACT_ERROR', message: failure.message || String(failure) } })
    }
  })
}

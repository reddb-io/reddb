// Test-only owned-store I/O; the production SDK's database API is still pending.
import init, { runResidentStoreFixture } from './wasm/pager.js'

self.onmessage = async ({ data: { phase } }) => {
  let access
  let interrupted = false
  try {
    await init()
    const root = await navigator.storage.getDirectory()
    const file = await root.getFileHandle('reddb-resident-store-conformance.rdb', { create: phase === 0 })
    access = await file.createSyncAccessHandle()
    const handle = {
      readAt: (buffer, offset) => access.read(buffer, { at: offset }),
      writeAt: (buffer, offset) => access.write(buffer, { at: offset }),
      getSize: () => access.getSize(),
      truncate: (size) => access.truncate(size),
      flush: () => {
        access.flush()
        if (phase === 1) {
          interrupted = true
          throw new Error('injected resident WAL flush failure')
        }
      },
    }
    const entities = runResidentStoreFixture(handle, phase)
    if (phase === 1) throw new Error('expected an interrupted resident WAL flush')
    access.close()
    access = undefined
    self.postMessage({ phase, entities })
  } catch (error) {
    access?.close()
    if (interrupted && error.message.includes('injected resident WAL flush failure')) {
      self.postMessage({ phase, interrupted: true })
    } else {
      self.postMessage({ error: error.message || String(error) })
    }
  }
}

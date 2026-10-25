// Test-only projection/WHERE/ORDER BY over recovered rows; public SQL dispatch is pending.
import init, { runRecordFilterFixture } from './wasm/pager.js'

self.onmessage = async ({ data: { phase } }) => {
  let access
  try {
    await init()
    const root = await navigator.storage.getDirectory()
    const file = await root.getFileHandle('reddb-record-filter-conformance.rdb', { create: phase === 0 })
    access = await file.createSyncAccessHandle()
    const counts = Array.from(runRecordFilterFixture({
      readAt: (buffer, offset) => access.read(buffer, { at: offset }),
      writeAt: (buffer, offset) => access.write(buffer, { at: offset }),
      getSize: () => access.getSize(),
      truncate: (size) => access.truncate(size),
      flush: () => access.flush(),
    }, phase))
    access.close()
    access = undefined
    self.postMessage({ phase, counts })
  } catch (error) {
    access?.close()
    self.postMessage({ error: error.message || String(error) })
  }
}

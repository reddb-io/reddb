// Test-only admission/publication fixture; the public SQL runtime is still pending.
import init, { runContractWriteFixture } from './wasm/pager.js'

self.onmessage = async ({ data: { phase } }) => {
  let access
  try {
    await init()
    const root = await navigator.storage.getDirectory()
    const file = await root.getFileHandle('reddb-contract-write-conformance.rdb', { create: phase === 0 })
    access = await file.createSyncAccessHandle()
    const total = runContractWriteFixture({
      readAt: (buffer, offset) => access.read(buffer, { at: offset }),
      writeAt: (buffer, offset) => access.write(buffer, { at: offset }),
      getSize: () => access.getSize(),
      truncate: (size) => access.truncate(size),
      flush: () => access.flush(),
    }, phase)
    access.close()
    access = undefined
    self.postMessage({ phase, total })
  } catch (error) {
    access?.close()
    self.postMessage({ error: error.message || String(error) })
  }
}

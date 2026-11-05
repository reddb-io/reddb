// Test-only worker; the production SDK does not expose raw B-tree operations.
import init, { runBtreeFixture } from './wasm/pager.js'

self.onmessage = async ({ data: { phase, fixture } }) => {
  let access
  try {
    await init()
    const root = await navigator.storage.getDirectory()
    const file = await root.getFileHandle('reddb-btree-conformance.rdb', { create: phase === 0 })
    access = await file.createSyncAccessHandle()
    if (phase === 0) {
      access.truncate(fixture.length)
      let offset = 0
      while (offset < fixture.length) {
        const count = access.write(fixture.subarray(offset), { at: offset })
        if (count <= 0) throw new Error('fixture import made no progress')
        offset += count
      }
      access.flush()
    }
    const handle = {
      readAt: (buffer, offset) => access.read(buffer, { at: offset }),
      writeAt: (buffer, offset) => access.write(buffer, { at: offset }),
      getSize: () => access.getSize(),
      truncate: (size) => access.truncate(size),
      flush: () => access.flush(),
    }
    // Phase 0 imports the native file; phase 1 mutates it; phase 2 verifies
    // those changes through a new browser/worker and exports the same file.
    runBtreeFixture(handle, phase === 0 ? 3 : phase)
    let bytes
    if (phase === 2) {
      bytes = new Uint8Array(access.getSize())
      let offset = 0
      while (offset < bytes.length) {
        const count = access.read(bytes.subarray(offset), { at: offset })
        if (count <= 0) throw new Error('fixture export ended early')
        offset += count
      }
    }
    access.close()
    access = undefined
    self.postMessage({ phase, bytes })
  } catch (failure) {
    access?.close()
    self.postMessage({ error: failure.message || String(failure) })
  }
}

import init, { pagerFixtureOffset, runPagerFixture } from './wasm/pager.js'

self.onmessage = async ({ data: { phase } }) => {
  let access
  let interrupted = false
  try {
    await init()
    const root = await navigator.storage.getDirectory()
    const file = await root.getFileHandle('reddb-pager-conformance.rdb', { create: phase === 0 })
    access = await file.createSyncAccessHandle()
    const handle = {
      readAt: (buffer, offset) => access.read(buffer, { at: offset }),
      writeAt: (buffer, offset) => {
        if (phase === 1 && offset === pagerFixtureOffset()) {
          // The staged DWB has already been flushed. Tear the home page and
          // fail the handle; recovery must happen on a fresh worker/handle.
          access.write(buffer.subarray(0, buffer.length / 2), { at: offset })
          access.flush()
          interrupted = true
          throw new Error('injected home-page write failure')
        }
        return access.write(buffer, { at: offset })
      },
      getSize: () => access.getSize(),
      truncate: (size) => access.truncate(size),
      flush: () => access.flush(),
    }
    runPagerFixture(handle, phase)
    if (phase === 1) throw new Error('expected an interrupted home-page write')
    access.close()
    access = undefined
    self.postMessage({ phase, recovered: phase === 2 })
  } catch (error) {
    access?.close()
    if (interrupted && error.message.includes('injected home-page write failure')) {
      self.postMessage({ phase, interrupted: true })
    } else {
      self.postMessage({ error: error.message || String(error) })
    }
  }
}

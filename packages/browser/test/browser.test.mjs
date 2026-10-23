import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { once } from 'node:events'
import { createServer } from 'node:http'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, extname, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { after, before, describe, test } from 'node:test'
import { chromium, firefox, webkit } from 'playwright'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..')
const toolchain = (await readFile(resolve(root, 'rust-toolchain.toml'), 'utf8')).match(/channel\s*=\s*"([^"]+)"/)[1]
const browserTypes = { chromium, firefox, webkit }
const browsers = (process.env.REDDB_BROWSER_BROWSERS || 'chromium,firefox,webkit').split(',')
let server
let origin
let fixtures
let nativeFixture
const networkRequests = []

async function withHostOffline(run) {
  // Also disconnect the HTTP listener: startup and recovery must succeed
  // without a reachable host, including across a complete browser restart.
  const { port } = server.address()
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()))
  try {
    await run()
  } finally {
    server.listen(port, '127.0.0.1')
    await once(server, 'listening')
  }
}

function nativeArtifact(action, path) {
  execFileSync('cargo', [`+${toolchain}`, 'run', '--quiet', '--locked', '-p', 'reddb-io-file', '--example', 'browser_artifact', '--', action, path], { cwd: root, stdio: 'inherit' })
}

before(async () => {
  fixtures = await mkdtemp(resolve(tmpdir(), 'reddb-browser-interop-'))
  const path = resolve(fixtures, 'native.rdb')
  nativeArtifact('create', path)
  nativeFixture = [...await readFile(path)]
  server = createServer(async (request, response) => {
    networkRequests.push(request.url)
    try {
      const path = resolve(root, `.${decodeURIComponent(new URL(request.url, 'http://localhost').pathname)}`)
      if (!path.startsWith(`${root}${sep}`)) { response.writeHead(403).end(); return }
      const type = { '.js': 'text/javascript', '.wasm': 'application/wasm', '.html': 'text/html' }[extname(path)] || 'application/octet-stream'
      const body = await readFile(path)
      const headers = { 'Content-Type': type }
      if (path === resolve(root, 'packages/browser/test/offline-worker.js')) {
        headers['Service-Worker-Allowed'] = '/packages/browser/'
      }
      response.writeHead(200, headers).end(body)
    } catch { response.writeHead(404).end() }
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  origin = `http://127.0.0.1:${server.address().port}`
})

after(async () => {
  if (server) await new Promise((resolve) => server.close(resolve))
  if (fixtures) await rm(fixtures, { recursive: true, force: true })
})

for (const browser of browsers) {
  describe(browser, () => {
    let context
    let profile
    let supportsStorage
    async function launch(options = {}) {
      context = await browserTypes[browser].launchPersistentContext(profile, { headless: true, ...options })
      const probe = await context.newPage()
      await probe.goto(`${origin}/packages/browser/test/harness.html`)
      supportsStorage = await probe.evaluate(async () => {
        if (!navigator.storage?.getDirectory) return false
        const url = URL.createObjectURL(new Blob([
          'postMessage(typeof globalThis.FileSystemFileHandle?.prototype.createSyncAccessHandle === "function")',
        ], { type: 'text/javascript' }))
        const worker = new Worker(url)
        try {
          return await new Promise((resolve, reject) => {
            worker.onmessage = ({ data }) => resolve(data)
            worker.onerror = reject
          })
        } finally {
          worker.terminate()
          URL.revokeObjectURL(url)
        }
      })
      await probe.close()
    }
    async function page() {
      const page = await context.newPage()
      await page.goto(`${origin}/packages/browser/test/harness.html`)
      await page.waitForFunction(() => window.RedDBBrowser)
      return page
    }

    before(async () => { profile = await mkdtemp(resolve(tmpdir(), 'reddb-browser-browser-')); await launch() })
    after(async () => { await context?.close(); await rm(profile, { recursive: true, force: true }) })

    function storageTest(name, run) {
      test(name, async (testContext) => {
        if (!supportsStorage) {
          testContext.skip('This browser port does not expose native synchronous OPFS; Safari/iOS qualification remains pending')
          return
        }
        await run()
      })
    }

    test('shared Rust query and storage conformance executes in WASM', async () => {
      const browserPage = await page()
      try {
        const passed = await browserPage.evaluate(async () => {
          const worker = new Worker('/packages/browser/test/query-worker.js', { type: 'module' })
          try {
            return await new Promise((resolve, reject) => {
              worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data.passed)
              worker.onerror = (event) => reject(new Error(event.message))
            })
          } finally { worker.terminate() }
        })
        assert.deepEqual(passed, [
          'scalar_goldens', 'scalar_failures', 'row_execution', 'binary_document_evaluation', 'join_execution',
          'aggregation_execution', 'window_execution', 'set_execution', 'cancellation',
          'page_layout_and_corruption', 'freelist_trunk_recovery', 'cache_eviction_and_writeback',
          'pager_barriers_and_restart', 'pager_failure_boundaries',
          'pager_frame_bounds_and_read_errors', 'pager_superblock_failure_boundaries',
          'pager_recovery_failure_boundaries',
        ])
      } finally { await browserPage.close() }
    })

    storageTest('worker executes scalar RQL and preserves exact integers and artifact state', async () => {
      const browserPage = await page()
      try {
        const result = await browserPage.evaluate(async () => {
          const cases = await (await fetch('/crates/reddb-engine/tests/scalar-cases.json')).json()
          const file = await RedDBBrowser.openArtifact({ name: 'scalar' })
          const generation = await file.generation()
          const actual = []
          for (const { sql } of cases) actual.push(await file.executeScalar(sql))
          const errors = []
          for (const sql of ['SELECT 1 FROM people', 'SELECT 1 / 0', 'SELECT FROM WHERE', 1]) {
            try { await file.executeScalar(sql) } catch (error) { errors.push(error.code) }
          }
          const unchanged = generation === await file.generation()
          const wal = await file.readWal()
          await file.close()
          return { actual, expected: cases.map(({ expected }) => expected), errors, unchanged, wal }
        })
        assert.deepEqual(result.actual, result.expected)
        assert.deepEqual(result.errors, ['QUERY_UNSUPPORTED', 'QUERY_EVALUATION_ERROR', 'QUERY_PARSE_ERROR', 'INVALID_ARGUMENT'])
        assert.equal(result.unchanged, true)
        assert.deepEqual(result.wal, [])
      } finally { await browserPage.close() }
    })

    storageTest('canonical RQL validation and OPFS writes survive browser restart', async () => {
      const first = await page()
      const result = await first.evaluate(async () => {
        const file = await RedDBBrowser.openArtifact({ name: 'restart' })
        await file.validateQuery('SELECT 1')
        let invalid
        try { await file.validateQuery('SELECT FROM WHERE') } catch (error) { invalid = error.code }
        await file.append([new Uint8Array([1, 2, 3]), new Uint8Array([4, 5, 6])])
        const generation = await file.generation()
        await file.close()
        return { generation, invalid }
      })
      assert.equal(result.invalid, 'ARTIFACT_ERROR')
      assert.equal(typeof result.generation, 'string')
      await context.close()
      await launch()
      const reopened = await page()
      assert.deepEqual(await reopened.evaluate(async () => {
        const file = await RedDBBrowser.openArtifact({ name: 'restart' })
        const payloads = (await file.readWal()).map((bytes) => [...bytes])
        await file.close()
        return payloads
      }), [[1, 2, 3], [4, 5, 6]])
    })

    storageTest('checkpoint, WAL growth, append and reopen preserve the native artifact', async () => {
      const current = await page()
      const result = await current.evaluate(async () => {
        let file = await RedDBBrowser.openArtifact({ name: 'checkpoint' })
        await file.append([new Uint8Array([7])])
        await file.checkpoint(new TextEncoder().encode('RDSTcheckpoint'), 128 * 1024)
        await file.append([new Uint8Array([8])])
        await file.close()
        file = await RedDBBrowser.openArtifact({ name: 'checkpoint' })
        const snapshot = new TextDecoder().decode(await file.readSnapshot())
        const wal = (await file.readWal()).map((bytes) => [...bytes])
        await file.close()
        const directory = await (await navigator.storage.getDirectory()).getDirectoryHandle('reddb-browser')
        const bytes = new Uint8Array(await (await (await directory.getFileHandle('checkpoint.rdb')).getFile()).arrayBuffer())
        return { snapshot, wal, magic: new TextDecoder().decode(bytes.slice(0, 8)) }
      })
      assert.deepEqual(result, { snapshot: 'RDSTcheckpoint', wal: [[8]], magic: 'RDBSBLK1' })
    })

    storageTest('a real native .rdb can be changed in the browser and reopened natively', async () => {
      const current = await page()
      const bytes = await current.evaluate(async (fixture) => {
        const directory = await (await navigator.storage.getDirectory()).getDirectoryHandle('reddb-browser', { create: true })
        const handle = await directory.getFileHandle('interop.rdb', { create: true })
        const writer = await handle.createWritable()
        await writer.write(new Uint8Array(fixture))
        await writer.close()
        const file = await RedDBBrowser.openArtifact({ name: 'interop' })
        await file.append([new TextEncoder().encode('browser')])
        await file.close()
        return [...new Uint8Array(await (await handle.getFile()).arrayBuffer())]
      }, nativeFixture)
      const path = resolve(fixtures, `${browser}.rdb`)
      await writeFile(path, Buffer.from(bytes))
      nativeArtifact('verify', path)
    })

    storageTest('a second tab cannot write concurrently, and owner teardown releases the lock', async () => {
      const owner = await page()
      const contender = await page()
      await owner.evaluate(async () => {
        window.file = await RedDBBrowser.openArtifact({ name: 'ownership' })
        await file.append([new Uint8Array([9])])
      })
      assert.equal(await contender.evaluate(async () => {
        try { await RedDBBrowser.openArtifact({ name: 'ownership' }); return 'unexpected success' }
        catch (error) { return error.code }
      }), 'FILE_BUSY')
      await owner.close()
      // Browser process teardown releases OPFS and Web Locks asynchronously.
      const recovered = await contender.evaluate(async () => {
        for (let attempt = 0; attempt < 100; attempt++) {
          try {
            const file = await RedDBBrowser.openArtifact({ name: 'ownership' })
            const payloads = (await file.readWal()).map((bytes) => [...bytes])
            await file.close()
            return payloads
          } catch (error) {
            if (error.code !== 'FILE_BUSY') throw error
            await new Promise((resolve) => setTimeout(resolve, 20))
          }
        }
        throw new Error('Owner teardown did not release the file')
      })
      assert.deepEqual(recovered, [[9]])
    })

    storageTest('a corrupt existing file is preserved, with no silent recreate', async () => {
      const current = await page()
      assert.deepEqual(await current.evaluate(async () => {
        const root = await navigator.storage.getDirectory()
        const directory = await root.getDirectoryHandle('reddb-browser', { create: true })
        const handle = await directory.getFileHandle('corrupt.rdb', { create: true })
        const writer = await handle.createWritable()
        await writer.write(new Uint8Array([77, 88, 99]))
        await writer.close()
        let code
        try { await RedDBBrowser.openArtifact({ name: 'corrupt' }) } catch (error) { code = error.code }
        return { code, bytes: [...new Uint8Array(await (await handle.getFile()).arrayBuffer())] }
      }), { code: 'ARTIFACT_ERROR', bytes: [77, 88, 99] })
    })

    storageTest('invalid arguments leave the artifact usable and close rejects new operations', async () => {
      const current = await page()
      assert.deepEqual(await current.evaluate(async () => {
        const file = await RedDBBrowser.openArtifact({ name: 'arguments' })
        let invalid
        try { await file.append(['not bytes']) } catch (error) { invalid = error.code }
        await file.append([new Uint8Array([10])])
        const closing = file.close()
        let closed
        try { await file.readWal() } catch (error) { closed = error.code }
        await closing
        await file.close()
        return { invalid, closed }
      }), { invalid: 'INVALID_ARGUMENT', closed: 'CLOSED' })
    })

    storageTest('cached JS and WASM boot, write and reopen after a browser restart entirely offline', async () => {
      const first = await page()
      await first.evaluate(async () => {
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
      })
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      await context.setOffline(true)
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          await first.evaluate(async () => {
            if (navigator.onLine) throw new Error('Test requires offline mode')
            const file = await RedDBBrowser.openArtifact({ name: 'offline' })
            await file.append([new Uint8Array([10, 20])])
            await file.close()
        })
        await context.close()
        await launch({ offline: true })
        const restarted = await page()
        const result = await restarted.evaluate(async () => {
          const file = await RedDBBrowser.openArtifact({ name: 'offline' })
          const before = (await file.readWal()).map((bytes) => [...bytes])
          await file.append([new Uint8Array([30, 40])])
          const scalar = await file.executeScalar('SELECT 6 * 7 AS answer')
          await file.close()
          const reopened = await RedDBBrowser.openArtifact({ name: 'offline' })
          const after = (await reopened.readWal()).map((bytes) => [...bytes])
          await reopened.close()
          return { online: navigator.onLine, before, after, scalar }
        })
        assert.deepEqual(result, {
          online: false,
          before: [[10, 20]],
          after: [[10, 20], [30, 40]],
          scalar: { columns: ['answer'], rows: [[42]] },
        })
        assert.equal(networkRequests.length, requestsBefore, `offline boot and operations reached the HTTP host: ${networkRequests.slice(requestsBefore).join(', ')}`)
        })
      } finally {
        await context.setOffline(false)
      }
    })

    storageTest('shared pager recovers a torn OPFS page after a full browser restart offline', async () => {
      async function fixture(phase) {
        const current = await page()
        try {
          return await current.evaluate(async (phase) => {
            const worker = new Worker('/packages/browser/test/pager-worker.js', { type: 'module' })
            try {
              return await new Promise((resolve, reject) => {
                worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data)
                worker.onerror = (event) => reject(new Error(event.message))
                worker.postMessage({ phase })
              })
            } finally { worker.terminate() }
          }, phase)
        } finally { await current.close() }
      }
      const first = await page()
      await first.evaluate(async () => {
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
      })
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      await first.close()
      await context.setOffline(true)
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          assert.deepEqual(await fixture(0), { phase: 0, recovered: false })
          assert.deepEqual(await fixture(1), { phase: 1, interrupted: true })
          await context.close()
          await launch({ offline: true })
          assert.deepEqual(await fixture(2), { phase: 2, recovered: true })
          assert.equal(networkRequests.length, requestsBefore, `offline pager recovery reached the HTTP host: ${networkRequests.slice(requestsBefore).join(', ')}`)
        })
      } finally {
        await context.setOffline(false)
      }
    })

    test('missing browser storage support returns an explicit error', async () => {
      const current = await page()
      assert.equal(await current.evaluate(async () => {
        if (navigator.storage) Object.defineProperty(navigator.storage, 'getDirectory', { value: undefined })
        try { await RedDBBrowser.openArtifact({ name: 'unsupported' }) }
        catch (error) { return error.code }
      }), 'STORAGE_UNSUPPORTED')
    })
  })
}

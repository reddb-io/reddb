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
let nativeTreeFixture
let nativeFixture
let nativeSnapshot
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

function nativeTree(action, path) {
  execFileSync('cargo', [`+${toolchain}`, 'run', '--quiet', '--locked', '-p', 'reddb-io-engine', '--example', 'browser_btree', '--', action, path], { cwd: root, stdio: 'inherit' })
}

async function sharedConformance(browserPage) {
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
    'btree_persisted_round_trip', 'btree_reclamation_and_cursor_lifetime', 'btree_bounds_and_corruption',
    'pager_orchestration_restart', 'pager_orchestration_allocation_fence', 'pager_orchestration_wal_barrier',
    'pager_orchestration_checkpoint_recovery', 'pager_orchestration_open_policy',
    'pager_barriers_and_restart', 'pager_failure_boundaries',
    'pager_multiframe_checkpoint_io_and_crash_recovery',
    'pager_frame_bounds_and_read_errors', 'pager_superblock_failure_boundaries',
    'pager_recovery_failure_boundaries',
    'mvcc_snapshot_isolation_and_read_committed', 'mvcc_failed_prepare_aborts_all_writers',
    'mvcc_publication_guard_covers_visibility', 'mvcc_savepoints_and_connection_isolation',
    'mvcc_retention_and_checkpoint_outcomes', 'mvcc_unique_keys_follow_outcomes',
    'mvcc_serializable_write_skew', 'mvcc_serializable_vault_predicates',
    'mvcc_autocommit_reservations_and_replay',
    'entity_models_and_metadata', 'entity_document_field_bloom', 'entity_auxiliary_and_timestamps',
    'entity_own_subtransaction_visibility', 'entity_logical_version_selection',
    'entity_serializable_logical_identity',
    'entity_codec_native_v1_v12_goldens', 'entity_codec_value_tags_and_truncation',
    'entity_codec_invalid_lengths_and_depth', 'entity_codec_optional_tails_and_truncation',
    'record_native_materialization_goldens', 'record_schema_and_graph_representation',
    'record_projection_and_document_body', 'record_typed_timeseries_and_identity',
    'segment_native_entity_shapes', 'segment_key_versions_and_mutation',
    'segment_bounded_scan_and_mutation', 'segment_metadata_and_sealing',
    'metadata_codec_native_goldens', 'metadata_codec_record_envelopes',
    'metadata_codec_malformed_payloads', 'metadata_codec_depth_boundary',
    'snapshot_native_store_fixture', 'snapshot_native_v1_v12_recovery', 'snapshot_empty_collections_and_legacy_tails',
    'snapshot_corruption_and_callback_errors',
    'manager_models_and_constraint_lock', 'manager_snapshot_key_versions',
    'manager_moderation_and_serializable_reads', 'manager_bounded_scan_and_consolidation',
    'manager_ordered_graph_candidates',
    'resident_index_document_tokens', 'resident_index_replacement_and_scope',
    'resident_entity_cache_invalidation',
    'resident_store_global_identity', 'resident_store_cross_collection_snapshot',
    'resident_store_models_and_cache', 'resident_store_graph_metadata_and_references',
  ])
}

before(async () => {
  fixtures = await mkdtemp(resolve(tmpdir(), 'reddb-browser-interop-'))
  const path = resolve(fixtures, 'native.rdb')
  nativeArtifact('create', path)
  nativeFixture = [...await readFile(path)]
  nativeSnapshot = [...await readFile(resolve(root, 'crates/reddb-engine/tests/fixtures/native-store-v12.rdst'))]
  const treePath = resolve(fixtures, 'native-tree.rdb')
  nativeTree('create', treePath)
  nativeTreeFixture = [...await readFile(treePath)]
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

    test('shared Rust query, storage and MVCC conformance executes in WASM', async () => {
      const browserPage = await page()
      try {
        await sharedConformance(browserPage)
      } finally { await browserPage.close() }
    })

    test('production runtime executes without storage, preserving canonical values and errors', async () => {
      const current = await page()
      try {
        const result = await current.evaluate(async () => {
          // An accidental storage dependency must fail, even on OPFS-capable ports.
          Object.defineProperty(navigator, 'storage', { value: undefined, configurable: true })
          Object.defineProperty(navigator, 'locks', { value: undefined, configurable: true })
          const runtime = await RedDBBrowser.createRuntime()
          try {
            const info = await runtime.info()
            const cases = await (await fetch('/crates/reddb-engine/tests/scalar-cases.json')).json()
            const actual = []
            for (const { sql } of cases) actual.push(await runtime.executeScalar(sql))
            await runtime.validateQuery('SELECT 1 FROM people')
            const errors = []
            for (const sql of ['SELECT 1 FROM people', 'SELECT 1 / 0', 'SELECT FROM WHERE', 1]) {
              try { await runtime.executeScalar(sql) } catch (error) { errors.push(error.code) }
            }
            return { info, actual, expected: cases.map(({ expected }) => expected), errors }
          } finally { await runtime.close() }
        })
        assert.deepEqual(result.info, {
          abiVersion: 1, valueEncoding: 'reddb-json-v1',
          capabilities: { queryValidation: true, scalarQueries: true, storedQueries: false, transactions: false, sync: false },
        })
        assert.deepEqual(result.actual, result.expected)
        assert.deepEqual(result.errors, ['QUERY_UNSUPPORTED', 'QUERY_EVALUATION_ERROR', 'QUERY_PARSE_ERROR', 'INVALID_ARGUMENT'])
      } finally { await current.close() }
    })

    test('worker rejects an old SDK ABI before creating an artifact', async () => {
      const current = await page()
      try {
        const result = await current.evaluate(async () => {
          const worker = new Worker('/packages/browser/src/worker.js', { type: 'module' })
          try {
            const reply = await new Promise((resolve, reject) => {
              worker.onmessage = ({ data }) => resolve(data)
              worker.onerror = event => reject(new Error(event.message))
              worker.postMessage({ abiVersion: 0, id: 1, method: 'open', params: { name: 'abi-mismatch' } })
            })
            let created = false
            if (navigator.storage?.getDirectory) {
              const root = await navigator.storage.getDirectory()
              try {
                const directory = await root.getDirectoryHandle('reddb-browser')
                await directory.getFileHandle('abi-mismatch.rdb')
                created = true
              } catch (error) { if (error.name !== 'NotFoundError') throw error }
            }
            return { code: reply.error.code, created }
          } finally { worker.terminate() }
        })
        assert.deepEqual(result, { code: 'VERSION_MISMATCH', created: false })
      } finally { await current.close() }
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

    storageTest('cached JS and WASM boot, run shared MVCC, write and reopen after a browser restart entirely offline', async () => {
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
        await sharedConformance(restarted)
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

    storageTest('native B-tree entities materialize and project across an offline WASM/browser restart, then reopen natively', async () => {
      async function fixture(phase) {
        const current = await page()
        try {
          return await current.evaluate(async ({ phase, fixture }) => {
            const worker = new Worker('/packages/browser/test/btree-worker.js', { type: 'module' })
            try {
              return await new Promise((resolve, reject) => {
                worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data)
                worker.onerror = (event) => reject(new Error(event.message))
                worker.postMessage({ phase, fixture: fixture ? Uint8Array.from(fixture) : undefined })
              })
            } finally { worker.terminate() }
          }, { phase, fixture: phase === 0 ? nativeTreeFixture : undefined })
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
          assert.equal((await fixture(0)).phase, 0)
          assert.equal((await fixture(1)).phase, 1)
          await context.close()
          await launch({ offline: true })
          const result = await fixture(2)
          const bytes = result.bytes instanceof Uint8Array ? result.bytes : Uint8Array.from(Object.values(result.bytes))
          const path = resolve(fixtures, `${browser}-tree.rdb`)
          await writeFile(path, bytes)
          nativeTree('verify', path)
          assert.equal(networkRequests.length, requestsBefore, `offline B-tree operations reached the HTTP host: ${networkRequests.slice(requestsBefore).join(', ')}`)
        })
      } finally {
        await context.setOffline(false)
      }
    })

    storageTest('native collections and typed metadata recover from an OPFS snapshot after an offline browser restart', async () => {
      async function recover(seed) {
        const current = await page()
        try {
          return await current.evaluate(async (seed) => {
            const file = await RedDBBrowser.openArtifact({ name: 'native-snapshot-recovery' })
            let snapshot
            try {
              if (seed) await file.checkpoint(Uint8Array.from(seed))
              snapshot = await file.readSnapshot()
            } finally { await file.close() }
            const worker = new Worker('/packages/browser/test/snapshot-worker.js', { type: 'module' })
            try {
              const entities = await new Promise((resolve, reject) => {
                worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data.entities)
                worker.onerror = (event) => reject(new Error(event.message))
                worker.postMessage(snapshot)
              })
              return { entities, snapshot: [...snapshot] }
            } finally { worker.terminate() }
          }, seed)
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
          assert.deepEqual(await recover(nativeSnapshot), { entities: 8, snapshot: nativeSnapshot })
          await context.close()
          await launch({ offline: true })
          assert.deepEqual(await recover(), { entities: 8, snapshot: nativeSnapshot })
          assert.equal(networkRequests.length, requestsBefore, 'offline snapshot recovery reached the HTTP host')
        })
      } finally { await context.setOffline(false) }
    })

    storageTest('shared pager recovers allocation and checkpoint state after a torn OPFS write offline', async () => {
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
          assert.deepEqual(await fixture(3), { phase: 3, recovered: false })
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

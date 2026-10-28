import assert from 'node:assert/strict'
import { after, before, test } from 'node:test'
import { once } from 'node:events'
import { createServer } from 'node:http'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, extname, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { chromium, firefox } from 'playwright'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..')
const prefix = '/packages/browser/src/'
const assets = ['/', ...['index.js', 'protocol.js', 'worker.js', 'wasm/reddb_wasm.js', 'wasm/reddb_wasm_bg.wasm'].map(name => prefix + name)]
const browsers = { chromium, firefox }
const selected = (process.env.REDDB_BROWSER_BROWSERS || 'chromium,firefox').split(',').filter(name => browsers[name])
let server, origin, temporary, requests = 0

before(async () => {
  temporary = await mkdtemp(resolve(tmpdir(), 'reddb-browser-session-'))
  server = createServer(async (request, response) => {
    requests++
    const url = new URL(request.url, 'http://localhost')
    if (url.pathname === '/') return response.writeHead(200, { 'Content-Type': 'text/html' }).end('<!doctype html><title>RedDB sessions</title>')
    if (url.pathname === '/offline.js') return response.writeHead(200, { 'Content-Type': 'text/javascript' }).end(`
const assets = ${JSON.stringify(assets)}
self.addEventListener('install', event => event.waitUntil(caches.open('session-test').then(cache => cache.addAll(assets))))
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()))
self.addEventListener('fetch', event => event.respondWith(caches.open('session-test').then(cache => cache.match(event.request).then(value => value || Response.error()))))
`)
    const path = resolve(root, `.${url.pathname}`)
    if (!path.startsWith(root + sep)) return response.writeHead(403).end()
    try {
      let body = await readFile(path)
      if (url.pathname === prefix + 'worker.js' && url.searchParams.has('faults')) body = Buffer.from(body.toString() + `
const originalWrite = FileSystemSyncAccessHandle.prototype.write
const originalClose = FileSystemSyncAccessHandle.prototype.close
let fault
FileSystemSyncAccessHandle.prototype.write = function(...args) { if (fault === 'write') throw new DOMException('Test disk full', 'QuotaExceededError'); return originalWrite.apply(this, args) }
FileSystemSyncAccessHandle.prototype.close = function(...args) { if (fault === 'close') throw Error('Test close failure'); return originalClose.apply(this, args) }
const dispatch = self.onmessage
self.onmessage = event => { if (event.data?.sessionTestFault) { fault = event.data.mode; event.ports[0].postMessage('ready'); return }; dispatch(event) }
`)
      response.writeHead(200, { 'Content-Type': extname(path) === '.wasm' ? 'application/wasm' : 'text/javascript', 'Cache-Control': 'public, max-age=3600' }).end(body)
    } catch { response.writeHead(404).end() }
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  origin = `http://127.0.0.1:${server.address().port}`
})
after(async () => {
  if (server?.listening) await new Promise(resolve => server.close(resolve))
  if (temporary) await rm(temporary, { recursive: true, force: true })
})

for (const name of selected) test(`${name}: scoped inspection is read-only and survives an offline browser restart`, { timeout: 120_000 }, async () => {
  const profile = await mkdtemp(resolve(temporary, 'inspection-'))
  let context
  const port = server.address().port
  async function inspect(page) {
    return page.evaluate(async prefix => {
      const { openDatabase } = await import(prefix + 'index.js')
      const db = await openDatabase({ name: 'inspection' })
      const scopes = [{ table: 'files', field: 'tenant_id', value: 'first' }]
      try {
        const catalog = await db.catalog()
        const result = await db.readQuery("SELECT body FROM files WHERE tenant_id = 'second' OR 1 = 1 ORDER BY id LIMIT 1", [], scopes)
        const denied = []
        for (const sql of ['DELETE FROM files', 'BEGIN', 'SELECT * FROM files; DELETE FROM files']) {
          try { await db.readQuery(sql, [], scopes); denied.push(false) } catch { denied.push(true) }
        }
        try { await db.readQuery('SELECT * FROM files', [], []); denied.push(false) } catch { denied.push(true) }
        return { catalog, result, denied, all: await db.query('SELECT body FROM files ORDER BY id') }
      } finally { await db.close() }
    }, prefix)
  }
  try {
    context = await browsers[name].launchPersistentContext(profile, { headless: true })
    let page = await context.newPage(); await page.goto(origin)
    await page.evaluate(async prefix => {
      const { openDatabase } = await import(prefix + 'index.js')
      const db = await openDatabase({ name: 'inspection', create: true })
      try {
        await db.query('CREATE TABLE files (id INTEGER PRIMARY KEY, tenant_id TEXT NOT NULL, body TEXT)')
        await db.query('INSERT INTO files VALUES (1, $1, $2), (2, $3, $4)', ['second', 'other campaign', 'first', 'my clue'])
      } finally { await db.close() }
    }, prefix)
    const expected = await inspect(page)
    assert.deepEqual(expected.result.rows, [['my clue']])
    assert.deepEqual(expected.denied, [true, true, true, true])
    assert.deepEqual(expected.all.rows, [['other campaign'], ['my clue']])
    assert.equal(expected.catalog.rows.length, 3)
    await page.evaluate(async () => { await navigator.serviceWorker.register('/offline.js'); await navigator.serviceWorker.ready })
    await page.waitForFunction(() => navigator.serviceWorker.controller)
    await context.close()
    await new Promise(resolve => server.close(resolve))
    const before = requests
    context = await browsers[name].launchPersistentContext(profile, { headless: true, offline: true })
    page = await context.newPage(); await page.goto(origin)
    assert.deepEqual(await inspect(page), expected)
    assert.equal(requests, before)
  } finally {
    await context?.close()
    if (!server.listening) { server.listen(port, '127.0.0.1'); await once(server, 'listening') }
  }
})

for (const name of selected) {
  test(`${name}: sessions release real file locks, recover peer commits and fence stale handles`, async () => {
    const browser = await browsers[name].launch({ headless: true })
    try {
      const context = await browser.newContext()
      const first = await context.newPage(), second = await context.newPage()
      await first.goto(origin); await second.goto(origin)
      await first.evaluate(async prefix => {
        const { createDatabaseSession } = await import(prefix + 'index.js')
        window.session = await createDatabaseSession()
        window.database = await session.openDatabase({ name: 'session-peer', create: true })
        await database.query('CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
        await database.query('INSERT INTO notes VALUES ($1, $2)', [1, 'committed'])
      }, prefix)
      const busy = await second.evaluate(async prefix => {
        const { createDatabaseSession } = await import(prefix + 'index.js')
        window.session = await createDatabaseSession()
        try { await session.openDatabase({ name: 'session-peer' }); return null }
        catch (error) { return error.code }
      }, prefix)
      assert.equal(busy, 'FILE_BUSY')
      await first.evaluate(async () => {
        await database.query('BEGIN')
        await database.query('UPDATE notes SET body = $1', ['uncommitted'])
        window.old = database
        await database.close()
      })
      const rows = await second.evaluate(async () => {
        const database = await session.openDatabase({ name: 'session-peer' })
        try {
          const rows = (await database.query('SELECT body FROM notes')).rows
          await database.query('UPDATE notes SET body = $1', ['peer commit'])
          return rows
        } finally { await database.close() }
      })
      assert.deepEqual(rows, [['committed']])
      const recovered = await first.evaluate(async () => {
        const database = await session.openDatabase({ name: 'session-peer' })
        try {
          let stale
          try { await old.query('DELETE FROM notes') } catch (error) { stale = error.code }
          await old.close()
          return { stale, rows: (await database.query('SELECT body FROM notes')).rows }
        } finally { await database.close(); await session.close() }
      })
      assert.deepEqual(recovered, { stale: 'CLOSED', rows: [['peer commit']] })
      await second.evaluate(() => session.close())
    } finally { await browser.close() }
  })

  test(`${name}: write and close failures preserve committed state and fence uncertain ownership`, async () => {
    const browser = await browsers[name].launch({ headless: true })
    try {
      const page = await browser.newPage()
      await page.goto(origin)
      const result = await page.evaluate(async prefix => {
        const { createDatabaseSession } = await import(prefix + 'index.js')
        const worker = new Worker(prefix + 'worker.js?faults', { type: 'module' })
        const session = await createDatabaseSession({ worker })
        const fault = async mode => {
          const channel = new MessageChannel()
          const ready = new Promise(resolve => { channel.port1.onmessage = resolve })
          worker.postMessage({ sessionTestFault: true, mode }, [channel.port2])
          await ready
          channel.port1.close()
        }
        let database = await session.openDatabase({ name: 'session-failure', create: true })
        await database.query('CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
        await database.query('INSERT INTO notes VALUES (1, $1)', ['before failure'])
        await fault('write')
        let writeFailed = false, readFenced = false
        try { await database.query('UPDATE notes SET body = $1', ['must not be acknowledged']) } catch { writeFailed = true }
        try { await database.query('SELECT body FROM notes') } catch { readFenced = true }
        await database.close()
        await fault(null)
        database = await session.openDatabase({ name: 'session-failure' })
        const afterWrite = (await database.query('SELECT body FROM notes')).rows
        await fault('close')
        let closeFailed = false, sessionFenced
        try { await database.close() } catch { closeFailed = true }
        try { await session.openDatabase({ name: 'session-failure' }) } catch (error) { sessionFenced = error.code }
        await session.close()
        return { writeFailed, readFenced, afterWrite, closeFailed, sessionFenced }
      }, prefix)
      assert.deepEqual(result, { writeFailed: true, readFenced: true, afterWrite: [['before failure']], closeFailed: true, sessionFenced: 'CLOSED' })
      await page.waitForFunction(async () => !(await navigator.locks.query()).held.some(lock => lock.name === 'reddb-browser:session-failure'))
      const rows = await page.evaluate(async prefix => {
        const { openDatabase } = await import(prefix + 'index.js')
        const database = await openDatabase({ name: 'session-failure' })
        try { return (await database.query('SELECT body FROM notes')).rows }
        finally { await database.close() }
      }, prefix)
      assert.deepEqual(rows, [['before failure']])
    } finally { await browser.close() }
  })

  test(`${name}: a reusable session recovers committed data after an offline browser restart with the host stopped`, async () => {
    const profile = resolve(temporary, name)
    let context = await browsers[name].launchPersistentContext(profile, { headless: true })
    const boot = page => page.evaluate(async prefix => {
      const { createDatabaseSession } = await import(prefix + 'index.js')
      const session = await createDatabaseSession()
      try {
        let database = await session.openDatabase({ name: 'session-offline', create: true })
        await database.query('CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
        const before = (await database.query('SELECT id FROM notes')).rows.length
        await database.query('INSERT INTO notes VALUES ($1, $2)', [before + 1, 'offline-' + before])
        await database.close()
        database = await session.openDatabase({ name: 'session-offline' })
        try { return (await database.query('SELECT body FROM notes ORDER BY id')).rows }
        finally { await database.close() }
      } finally { await session.close() }
    }, prefix)
    try {
      let page = await context.newPage()
      await page.goto(origin)
      assert.deepEqual(await boot(page), [['offline-0']])
      await page.evaluate(() => navigator.serviceWorker.register('/offline.js'))
      await page.waitForFunction(() => navigator.serviceWorker.controller)
      await context.close()
      const { port } = server.address()
      await new Promise(resolve => server.close(resolve))
      try {
        const before = requests
        for (const count of [2, 3]) {
          context = await browsers[name].launchPersistentContext(profile, { headless: true, offline: true })
          page = await context.newPage()
          await page.goto(origin)
          assert.deepEqual(await boot(page), Array.from({ length: count }, (_, index) => ['offline-' + index]))
          await context.close()
        }
        assert.equal(requests, before)
      } finally { server.listen(port, '127.0.0.1'); await once(server, 'listening') }
    } finally { await context.close() }
  })
}

test('Chromium: compare fresh Workers and reusable sessions for the same 300 kB committed write', { skip: !process.env.REDDB_BROWSER_BENCHMARK, timeout: 120000 }, async () => {
  const browser = await chromium.launch({ headless: true })
  try {
    const page = await browser.newPage()
    await page.goto(origin)
    const result = await page.evaluate(async prefix => {
      const { createDatabaseSession, openDatabase } = await import(prefix + 'index.js')
      const payload = 'x'.repeat(300000), fresh = [], reused = []
      const session = await createDatabaseSession()
      const prepare = async (db, index) => {
        await db.query('CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
        await db.query('BEGIN')
        await db.query('DELETE FROM notes WHERE id = 1')
        await db.query('INSERT INTO notes VALUES (1, $1)', [payload + index])
        await db.query('COMMIT')
      }
      try {
        for (let index = 0; index < 12; index++) {
          for (const mode of index % 2 ? ['reused', 'fresh'] : ['fresh', 'reused']) {
            const start = performance.now()
            const db = mode === 'fresh' ? await openDatabase({ name: 'benchmark-fresh', create: true }) : await session.openDatabase({ name: 'benchmark-reused', create: true })
            try { await prepare(db, index) } finally { await db.close() }
            if (index > 1) (mode === 'fresh' ? fresh : reused).push(performance.now() - start)
          }
        }
      } finally { await session.close() }
      return { fresh, reused }
    }, prefix)
    console.log('300 kB open / commit / close, same file history and alternating order (ms):', result)
  } finally { await browser.close() }
})

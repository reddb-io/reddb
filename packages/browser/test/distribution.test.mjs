import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { once } from 'node:events'
import { createServer } from 'node:http'
import { mkdtemp, readFile, writeFile, rm, readdir } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, extname, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { after, before, test } from 'node:test'
import { build } from 'vite'
import { chromium, firefox } from 'playwright'

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const root = resolve(packageRoot, '../..')
let temporary
let server
let origin
let distribution
let requests = 0

async function files(directory, prefix = '') {
  const result = []
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const name = `${prefix}${entry.name}`
    if (entry.isDirectory()) result.push(...await files(resolve(directory, entry.name), `${name}/`))
    else result.push(name)
  }
  return result
}

before(async () => {
  temporary = await mkdtemp(resolve(tmpdir(), 'reddb-browser-package-'))
  const source = process.env.REDDB_BROWSER_PACKAGE
  const packed = JSON.parse(execFileSync('npm', ['pack', ...(source ? [source] : []), '--json', '--pack-destination', temporary], { cwd: packageRoot, encoding: 'utf8' }))[0]
  const included = packed.files.map(({ path }) => path)
  for (const file of ['src/index.js', 'src/index.d.ts', 'src/protocol.js', 'src/worker.js', 'src/wasm/reddb_wasm.js', 'src/wasm/reddb_wasm_bg.wasm', 'LICENSE']) assert.ok(included.includes(file), `package is missing ${file}`)
  assert.ok(!included.some((path) => path.startsWith('test/') || path.includes('conformance')), 'test-only engine artifacts must not ship')
  await writeFile(resolve(temporary, 'package.json'), JSON.stringify({ name: 'browser-consumer', private: true, type: 'module' }))
  execFileSync('npm', ['install', '--ignore-scripts', '--package-lock=false', '--no-audit', '--no-fund', resolve(temporary, packed.filename)], { cwd: temporary, stdio: 'pipe' })
  await writeFile(resolve(temporary, 'index.html'), '<!doctype html><meta charset="utf-8"><title>RedDB package consumer</title><script type="module" src="/main.ts"></script>')
  await writeFile(resolve(temporary, 'worker.d.ts'), "declare module '*?worker' { const Constructor: { new(): Worker }; export default Constructor }")
  await writeFile(resolve(temporary, 'main.ts'), `
import { createRuntime, createInteropRuntime, openArtifact, openDatabase } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'
declare global { interface Window { result: Promise<unknown> } }
window.result = (async () => {
  const runtime = await createRuntime({ worker: new RedDBWorker() })
  try {
    const info = await runtime.info()
    if (info.abiVersion !== 1 || !info.capabilities.storedQueries || info.capabilities.storedQueryMode !== 'read-only-tables') throw new Error('Incorrect runtime capabilities')
    const result = await runtime.executeScalar('SELECT 6 * 7 AS answer')
    if (result.rows[0][0] !== 42) throw new Error('Incorrect scalar result')
  } finally { await runtime.close() }
  const bridge = createInteropRuntime({ worker: new RedDBWorker() })
  try {
    const result = await bridge.invoke('executeScalar', { sql: 'SELECT * FROM people' })
    if (result.ok || result.error.code !== 'QUERY_UNSUPPORTED') throw new Error('Incorrect interop error')
  } finally { bridge.close() }
  const artifact = await openArtifact({ name: 'packed-consumer', worker: new RedDBWorker() })
  let before, scalar
  try {
    before = (await artifact.readWal()).map((bytes) => [...bytes])
    scalar = await artifact.executeScalar('SELECT 6 * 7 AS answer')
    await artifact.append([new Uint8Array([before.length + 1])])
  } finally { await artifact.close() }
  const directory = await (await navigator.storage.getDirectory()).getDirectoryHandle('reddb-browser')
  try { await directory.getFileHandle('packed-query.rdb') }
  catch (error) {
    if (!(error instanceof DOMException) || error.name !== 'NotFoundError') throw error
    const writer = await (await directory.getFileHandle('packed-query.rdb', { create: true })).createWritable()
    await writer.write(await (await fetch('/native.rdb')).arrayBuffer())
    await writer.close()
  }
  const database = await openDatabase({ name: 'packed-query', worker: new RedDBWorker() })
  try {
    const stored = await database.query('SELECT rid, name, total FROM people WHERE total >= 10 ORDER BY total DESC')
    return { before, scalar, stored }
  } finally { await database.close() }
})()
`)
  const tsc = resolve(packageRoot, 'node_modules/typescript/bin/tsc')
  execFileSync(process.execPath, [tsc, '--noEmit', '--strict', '--module', 'ESNext', '--moduleResolution', 'Bundler', '--target', 'ES2022', '--lib', 'ES2022,DOM', resolve(temporary, 'worker.d.ts'), resolve(temporary, 'main.ts')], { cwd: temporary, stdio: 'pipe' })
  distribution = resolve(temporary, 'dist')
  await build({ configFile: false, root: temporary, logLevel: 'warn', build: { outDir: distribution } })
  const toolchain = (await readFile(resolve(root, 'rust-toolchain.toml'), 'utf8')).match(/channel\s*=\s*"([^"]+)"/)[1]
  execFileSync('cargo', ['+' + toolchain, 'run', '--quiet', '--locked', '-p', 'reddb-io-engine', '--example', 'browser_stored_query', '--', resolve(distribution, 'native.rdb')], { cwd: root, stdio: 'inherit' })
  const assets = ['/', ...await files(distribution)].map((path) => path.startsWith('/') ? path : `/${path}`)
  assert.ok(assets.some((path) => path.endsWith('.wasm')), 'bundler must emit the WASM asset')
  await writeFile(resolve(distribution, 'offline.js'), `
const assets = ${JSON.stringify(assets)}
self.addEventListener('install', event => event.waitUntil(caches.open('packed-browser').then(cache => cache.addAll(assets))))
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()))
self.addEventListener('fetch', event => event.respondWith(caches.open('packed-browser').then(async cache => await cache.match(event.request) || Response.error())))
`)
  server = createServer(async (request, response) => {
    requests++
    const url = new URL(request.url, 'http://localhost')
    const path = resolve(distribution, `.${url.pathname === '/' ? '/index.html' : url.pathname}`)
    if (!path.startsWith(`${distribution}${sep}`)) { response.writeHead(403).end(); return }
    try {
      const type = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm' }[extname(path)] || 'application/octet-stream'
      const contents = await readFile(path)
      response.writeHead(200, { 'Content-Type': type }).end(contents)
    } catch { response.writeHead(404).end() }
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  origin = `http://127.0.0.1:${server.address().port}`
})

after(async () => {
  if (server?.listening) await new Promise((resolve) => server.close(resolve))
  if (temporary) await rm(temporary, { recursive: true, force: true })
})

for (const [name, browser] of Object.entries({ chromium, firefox })) {
  test(`${name}: installed npm tarball builds in TypeScript/Vite and reopens OPFS offline`, async () => {
    const profile = resolve(temporary, `${name}-profile`)
    let context = await browser.launchPersistentContext(profile, { headless: true })
    try {
      let page = await context.newPage()
      await page.goto(origin)
      assert.deepEqual(await page.evaluate(() => window.result), { before: [], scalar: { columns: ['answer'], rows: [[42]] }, stored: { columns: ['rid', 'name', 'total'], rows: [[2, 'Bia', 22], [{ $uint: '9007199254740993' }, 'Ana', 10]] } })
      await page.evaluate(() => navigator.serviceWorker.register('/offline.js'))
      await page.waitForFunction(() => navigator.serviceWorker.controller)
      await context.close()
      const { port } = server.address()
      await new Promise((resolve) => server.close(resolve))
      try {
        const before = requests
        context = await browser.launchPersistentContext(profile, { headless: true, offline: true })
        page = await context.newPage()
        await page.goto(origin)
        assert.deepEqual(await page.evaluate(() => window.result), { before: [[1]], scalar: { columns: ['answer'], rows: [[42]] }, stored: { columns: ['rid', 'name', 'total'], rows: [[2, 'Bia', 22], [{ $uint: '9007199254740993' }, 'Ana', 10]] } })
        assert.equal(requests, before, 'offline consumer must not reach the HTTP host')
      } finally {
        server.listen(port, '127.0.0.1')
        await once(server, 'listening')
      }
    } finally { await context.close() }
  })
}

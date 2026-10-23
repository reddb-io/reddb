import assert from 'node:assert/strict'
import { once } from 'node:events'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { dirname, extname, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { after, before, test } from 'node:test'
import { chromium, firefox } from 'playwright'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const output = resolve(root, 'dist')
const prefix = '/reddb/browser/'
let temporary
let server
let origin = process.env.REDDB_BROWSER_EXAMPLE_URL
let requests = 0

before(async () => {
  temporary = await mkdtemp(resolve(tmpdir(), 'reddb-browser-example-'))
  if (origin) return
  server = createServer(async (request, response) => {
    requests++
    const pathname = new URL(request.url, 'http://localhost').pathname
    if (!pathname.startsWith(prefix)) { response.writeHead(404).end(); return }
    const relative = pathname.slice(prefix.length) || 'index.html'
    const path = resolve(output, relative)
    if (!path.startsWith(`${output}${sep}`)) { response.writeHead(403).end(); return }
    try {
      const body = await readFile(path)
      const type = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.wasm': 'application/wasm' }[extname(path)] || 'application/octet-stream'
      response.writeHead(200, { 'Content-Type': type }).end(body)
    } catch { response.writeHead(404).end() }
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  origin = `http://127.0.0.1:${server.address().port}${prefix}`
})

after(async () => {
  if (server?.listening) await new Promise(resolve => server.close(resolve))
  if (temporary) await rm(temporary, { recursive: true, force: true })
})

async function ready(page) {
  await page.goto(origin)
  await page.waitForFunction(() => document.querySelector('#engine-state').textContent.includes('WASM ativo'))
  await page.waitForFunction(() => !document.querySelector('#reload').disabled)
  await page.waitForFunction(() => document.querySelector('#query-result').textContent.includes('42'))
  assert.deepEqual(JSON.parse(await page.locator('#query-result').textContent()), { columns: ['resposta'], rows: [[42]] })
}

for (const [name, browser] of Object.entries({ chromium, firefox })) {
  test(`${name}: published-package UI, errors, ownership and a full offline restart`, async () => {
    const profile = resolve(temporary, `${name}-profile`)
    let context = await browser.launchPersistentContext(profile, { headless: true })
    try {
      let page = await context.newPage()
      await ready(page)
      assert.equal(await page.locator('#record-count').textContent(), '0')
      await page.getByRole('button', { name: 'Texto', exact: true }).click()
      await page.getByRole('button', { name: 'Executar consulta' }).click()
      await page.waitForFunction(() => document.querySelector('#query-result').textContent.includes('sem rede'))
      assert.deepEqual(JSON.parse(await page.locator('#query-result').textContent()), { columns: ['modo', 'resposta'], rows: [['sem rede', 42]] })
      await page.getByRole('button', { name: 'Limite atual' }).click()
      await page.getByRole('button', { name: 'Executar consulta' }).click()
      await page.waitForFunction(() => !document.querySelector('#error').hidden)
      assert.match(await page.locator('#error').textContent(), /QUERY_UNSUPPORTED/)
      await page.getByRole('button', { name: 'Cálculo', exact: true }).click()
      await page.getByRole('button', { name: 'Executar consulta' }).click()
      await page.waitForFunction(() => document.querySelector('#query-result').textContent.includes('42'))
      assert.equal(await page.locator('#error').isHidden(), true)
      await page.getByRole('button', { name: 'Gravar marcador' }).click()
      await page.waitForFunction(() => document.querySelector('#record-count').textContent === '1')
      await page.getByRole('button', { name: 'Reabrir arquivo' }).click()
      await page.waitForFunction(() => document.querySelector('#storage-status').textContent.includes('Arquivo reaberto'))
      assert.equal(await page.locator('#record-count').textContent(), '1')
      const second = await context.newPage()
      await second.goto(origin)
      await second.waitForFunction(() => !document.querySelector('#error').hidden)
      assert.match(await second.locator('#error').textContent(), /FILE_BUSY/)
      assert.equal(await second.getByRole('button', { name: 'Gravar marcador' }).isDisabled(), true)
      await second.close()
      await page.setViewportSize({ width: 390, height: 844 })
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)
      await context.close()
      const port = server?.address().port
      if (server) await new Promise(resolve => server.close(resolve))
      const before = requests
      try {
        context = await browser.launchPersistentContext(profile, { headless: true, offline: true })
        page = await context.newPage()
        await ready(page)
        assert.equal(await page.locator('#record-count').textContent(), '1')
        await page.getByRole('button', { name: 'Gravar marcador' }).click()
        await page.waitForFunction(() => document.querySelector('#record-count').textContent === '2')
        await page.getByRole('button', { name: 'Reabrir arquivo' }).click()
        await page.waitForFunction(() => document.querySelector('#storage-status').textContent.includes('Arquivo reaberto'))
        assert.equal(await page.locator('#record-count').textContent(), '2')
        assert.equal(requests, before, 'offline restart must not reach the HTTP host')
      } finally {
        if (server) { server.listen(port, '127.0.0.1'); await once(server, 'listening') }
      }
    } finally { await context.close() }
  })
}

test('unsupported storage remains explicit and does not enable write controls', async () => {
  const browser = await chromium.launch({ headless: true })
  try {
    const context = await browser.newContext()
    await context.addInitScript(() => Object.defineProperty(navigator.storage, 'getDirectory', { value: undefined }))
    const page = await context.newPage()
    await page.goto(origin)
    await page.waitForFunction(() => !document.querySelector('#error').hidden)
    assert.match(await page.locator('#error').textContent(), /STORAGE_UNSUPPORTED/)
    assert.equal(await page.getByRole('button', { name: 'Gravar marcador' }).isDisabled(), true)
    assert.equal(await page.getByRole('button', { name: 'Executar consulta' }).isDisabled(), true)
  } finally { await browser.close() }
})

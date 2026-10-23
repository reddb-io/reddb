import assert from 'node:assert/strict'
import AxeBuilder from '@axe-core/playwright'
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
  await page.waitForFunction(() => document.querySelector('#query-result code').textContent.includes('42'))
  assert.deepEqual(JSON.parse(await page.locator('#query-result code').textContent()), { columns: ['resposta'], rows: [[42]] })
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
      await page.waitForFunction(() => document.querySelector('#query-result code').textContent.includes('sem rede'))
      assert.deepEqual(JSON.parse(await page.locator('#query-result code').textContent()), { columns: ['modo', 'resposta'], rows: [['sem rede', 42]] })
      await page.getByRole('button', { name: 'Limite atual' }).click()
      await page.getByRole('button', { name: 'Executar consulta' }).click()
      await page.waitForFunction(() => !document.querySelector('#error').hidden)
      assert.match(await page.locator('#error').textContent(), /QUERY_UNSUPPORTED/)
      await page.getByRole('button', { name: 'Cálculo', exact: true }).click()
      await page.getByRole('button', { name: 'Executar consulta' }).click()
      await page.waitForFunction(() => document.querySelector('#query-result code').textContent.includes('42'))
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


test('official DS marks/fonts remain byte-identical and light/dark layouts pass WCAG AA', async () => {
  const browser = await chromium.launch({ headless: true })
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } })
    const page = await context.newPage()
    await ready(page)
    const backgrounds = []
    for (const scheme of ['light', 'dark']) {
      await page.evaluate(value => { document.documentElement.dataset.colorScheme = value }, scheme)
      await page.waitForFunction(value => document.querySelector('[data-logo-on]').dataset.logoOn === value, scheme)
      await page.evaluate(() => document.fonts.ready)
      const logo = page.locator('[data-logo-mark]:visible')
      const file = await logo.getAttribute('data-logo-mark')
      const image = await page.request.get(await logo.locator('img').getAttribute('src'))
      assert.equal(image.ok(), true)
      assert.deepEqual(await image.body(), await readFile(resolve(root, '../../vendor/ds/kits/base/src/marks', file)))
      backgrounds.push(await page.evaluate(() => getComputedStyle(document.body).backgroundColor))
      for (const width of [1440, 390]) {
        await page.setViewportSize({ width, height: 1000 })
        const audit = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa']).analyze()
        assert.deepEqual(audit.violations.map(({ id, nodes }) => ({ id, nodes: nodes.map(({ target }) => target) })), [], `${scheme} / ${width}px`)
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)
      }
    }
    assert.notEqual(backgrounds[0], backgrounds[1], 'both DS color schemes must be active')
    const fonts = await page.evaluate(() => performance.getEntriesByType('resource').map(entry => entry.name).filter(name => name.endsWith('.woff2')))
    for (const family of ['space-grotesk-variable', 'jetbrains-mono-variable']) {
      const url = fonts.find(name => name.includes(family))
      assert.ok(url, `${family} must be served with the application`)
      const response = await page.request.get(url)
      assert.equal(response.ok(), true)
      assert.deepEqual(await response.body(), await readFile(resolve(root, '../../vendor/ds/assets/fonts', `${family}.woff2`)))
    }
    for (const density of ['tiny', 'spacious']) {
      await page.evaluate(value => { document.documentElement.dataset.density = value }, density)
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)
    }
  } finally { await browser.close() }
})


test('workspace keeps query/result peers aligned, adapts to its container and supports keyboard navigation', async () => {
  const browser = await chromium.launch({ headless: true })
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } })
    const page = await context.newPage()
    await ready(page)
    assert.equal(await page.locator('main').count(), 1)
    assert.equal(await page.getByRole('heading', { level: 1 }).count(), 1)
    await page.keyboard.press('Tab')
    await page.keyboard.press('Enter')
    assert.equal(await page.evaluate(() => document.activeElement.id), 'main-content')
    const editor = () => page.locator('#sql').boundingBox()
    const result = () => page.locator('#query-result').boundingBox()
    const content = () => page.locator('[data-sidebar-layout-region="main"]').boundingBox()
    const sidebar = () => page.locator('[data-sidebar-layout-region="sidebar"]').boundingBox()
    let [e, r, c, s] = await Promise.all([editor(), result(), content(), sidebar()])
    assert.ok(e.y < 400, 'the editor must be visible in the first desktop viewport')
    assert.ok(Math.abs(e.y - r.y) <= 2, 'editor and result share their top boundary')
    assert.ok(r.x >= e.x + e.width, 'query/result share the desktop workspace horizontally')
    assert.ok(s.x >= c.x + c.width, 'artifact controls sit beside the workspace')
    const switchControl = page.getByRole('switch', { name: 'Tema escuro' })
    await switchControl.focus()
    await page.keyboard.press('Space')
    assert.equal(await page.locator('html').getAttribute('data-color-scheme'), 'dark')
    await page.setViewportSize({ width: 390, height: 844 })
    ;[e, r, c, s] = await Promise.all([editor(), result(), content(), sidebar()])
    assert.ok(r.y >= e.y + e.height, 'mobile result follows the editor')
    assert.ok(s.y >= c.y + c.height, 'mobile file controls follow the query workspace')
    await page.getByRole('button', { name: 'Menu', exact: true }).click()
    assert.equal(await page.getByRole('switch', { name: 'Tema escuro' }).isChecked(), true)
    assert.equal(await page.getByRole('link', { name: 'Guia JS/TS', exact: true }).isVisible(), true)
    await page.keyboard.press('Escape')
    assert.equal(await page.getByRole('button', { name: 'Menu', exact: true }).getAttribute('aria-expanded'), 'false')
    await page.getByRole('button', { name: 'Como testar offline', exact: true }).click()
    assert.equal(await page.getByText('Desligue a rede do navegador.', { exact: true }).isVisible(), true)
    await page.setViewportSize({ width: 1440, height: 1000 })
    await page.locator('.playground-workspace').evaluate(element => { element.style.width = '640px' })
    ;[e, r, c, s] = await Promise.all([editor(), result(), content(), sidebar()])
    assert.ok(r.y >= e.y + e.height, 'a narrow container stacks the editor on a wide viewport')
    assert.ok(s.y >= c.y + c.height, 'a narrow container stacks the sidebar on a wide viewport')
  } finally { await browser.close() }
})

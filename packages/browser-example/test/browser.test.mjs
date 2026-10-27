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
let updateRevision = false

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
      let body = await readFile(path)
      if (relative === 'service-worker.js' && updateRevision) {
        body = Buffer.from(body.toString().replace(/(const current = prefix \+ '[^']*)'/, "$1-update-test'"))
      }
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
  await page.getByRole('tab', { name: 'JSON', exact: true }).click()
  const result = await response(page)
  assert.deepEqual(result.columns, ['id', 'body'])
  assert.equal(Array.isArray(result.rows), true)
}

async function response(page) {
  return JSON.parse(await page.locator('#query-result code').textContent())
}

async function execute(page, sql, fails = false, parameters = []) {
  await page.getByRole('textbox', { name: 'Consulta RQL' }).fill(sql)
  await page.getByRole('textbox', { name: 'Parâmetros (JSON)', exact: true }).fill(JSON.stringify(parameters))
  await page.getByRole('button', { name: 'Executar consulta' }).click()
  await page.waitForFunction(() => !document.querySelector('#execute').disabled)
  assert.equal(await page.locator('#error').isVisible(), fails, await page.locator('#error').textContent())
  return fails ? await page.locator('#error').textContent() : await response(page)
}

async function preset(page, label) {
  await page.getByRole('button', { name: label, exact: true }).click()
  return execute(page, await page.locator('#sql').inputValue(), false, JSON.parse(await page.locator('#parameters').inputValue()))
}

async function transaction(page, command, fails = false) {
  await page.getByRole('button', { name: command, exact: true }).click()
  await page.waitForFunction(() => !document.querySelector('#execute').disabled)
  assert.equal(await page.locator('#error').isVisible(), fails, await page.locator('#error').textContent())
  return fails ? await page.locator('#error').textContent() : await response(page)
}

for (const [name, browser] of Object.entries({ chromium, firefox })) {
  test(`${name}: local SQL CRUD, transactions, ownership and two full offline restarts`, async () => {
    const profile = resolve(temporary, `${name}-profile`)
    let context = await browser.launchPersistentContext(profile, { headless: true })
    try {
      let page = await context.newPage()
      await page.route(origin, route => route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Storage setup</title>' }))
      await page.goto(origin)
      await page.evaluate(async () => {
        const directory = await (await navigator.storage.getDirectory()).getDirectoryHandle('reddb-browser', { create: true })
        const file = await directory.getFileHandle('reddb-browser-playground-v1.rdb', { create: true })
        const writer = await file.createWritable()
        await writer.write(new Uint8Array([82, 68, 66, 1]))
        await writer.close()
      })
      await page.unroute(origin)
      await ready(page)
      assert.deepEqual((await response(page)).rows, [])
      await preset(page, 'Criar tabela')
      assert.deepEqual(await preset(page, 'Inserir'), { columns: ['affectedRows'], rows: [[1]] })
      assert.deepEqual((await preset(page, 'Ler')).rows, [[1, 'Minha nota offline']])
      await preset(page, 'Alterar')
      assert.deepEqual((await preset(page, 'Ler')).rows, [[1, 'Nota atualizada offline']])
      await preset(page, 'Excluir')
      assert.deepEqual((await preset(page, 'Ler')).rows, [])
      await preset(page, 'Inserir')

      await transaction(page, 'BEGIN')
      await execute(page, 'CREATE TABLE history (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
      await execute(page, "INSERT INTO history VALUES (1, 'Catálogo offline')")
      await transaction(page, 'COMMIT')
      assert.deepEqual((await execute(page, 'SELECT id, body FROM history')).rows, [[1, 'Catálogo offline']])

      await transaction(page, 'BEGIN')
      await execute(page, "INSERT INTO notes VALUES (2, 'Pendente')")
      assert.equal((await preset(page, 'Ler')).rows.length, 2)
      await transaction(page, 'ROLLBACK')
      assert.deepEqual((await preset(page, 'Ler')).rows, [[1, 'Minha nota offline']])
      await transaction(page, 'BEGIN')
      await execute(page, "UPDATE notes SET body = 'Confirmada offline' WHERE id = 1")
      await transaction(page, 'COMMIT')
      assert.match(await page.locator('#storage-status').textContent(), /confirmadas no OPFS/)

      await transaction(page, 'BEGIN')
      await execute(page, "UPDATE notes SET body = 'Descartar ao reabrir' WHERE id = 1")
      await page.getByRole('button', { name: 'Reabrir banco' }).click()
      await page.waitForFunction(() => document.querySelector('#storage-status').textContent.includes('Banco reaberto'))
      assert.deepEqual((await response(page)).rows, [[1, 'Confirmada offline']])

      await transaction(page, 'BEGIN')
      await execute(page, "INSERT INTO notes VALUES (1, 'Duplicada')", true)
      assert.match(await transaction(page, 'COMMIT', true), /ROLLBACK/)
      assert.match(await execute(page, 'SELECT id, body FROM notes', true), /ROLLBACK/)
      await transaction(page, 'ROLLBACK')
      assert.deepEqual((await preset(page, 'Ler')).rows, [[1, 'Confirmada offline']])
      assert.match(await execute(page, 'SELECT COUNT(*) FROM notes', true), /QUERY_UNSUPPORTED/)
      await preset(page, 'Ler')

      const second = await context.newPage()
      await second.goto(origin)
      await second.waitForFunction(() => !document.querySelector('#error').hidden)
      assert.match(await second.locator('#error').textContent(), /FILE_BUSY/)
      assert.equal(await second.getByRole('button', { name: 'BEGIN', exact: true }).isDisabled(), true)
      assert.equal(await second.getByRole('button', { name: 'Executar consulta' }).isDisabled(), true)
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
        assert.deepEqual((await response(page)).rows, [[1, 'Confirmada offline']])
        await execute(page, 'UPDATE notes SET body = $1 WHERE id = $2', false, ['Gravada sem rede', 1])
        await page.reload()
        await page.getByRole('gridcell', { name: 'Gravada sem rede', exact: true }).waitFor()
        await page.getByRole('tab', { name: 'JSON', exact: true }).click()
        await transaction(page, 'BEGIN')
        await execute(page, "INSERT INTO notes VALUES (2, 'Descartar ao fechar')")
        await context.close()
        context = await browser.launchPersistentContext(profile, { headless: true, offline: true })
        page = await context.newPage()
        await ready(page)
        assert.deepEqual((await response(page)).rows, [[1, 'Gravada sem rede']])
        assert.deepEqual((await execute(page, 'SELECT id, body FROM history')).rows, [[1, 'Catálogo offline']])
        const legacy = await page.evaluate(async () => {
          const directory = await (await navigator.storage.getDirectory()).getDirectoryHandle('reddb-browser')
          const file = await (await directory.getFileHandle('reddb-browser-playground-v1.rdb')).getFile()
          return [...new Uint8Array(await file.arrayBuffer())]
        })
        assert.deepEqual(legacy, [82, 68, 66, 1], 'the old marker file must remain byte-identical')
        assert.equal(requests, before, 'offline restart must not reach the HTTP host')
      } finally {
        if (server) { server.listen(port, '127.0.0.1'); await once(server, 'listening') }
      }
    } finally { await context.close() }
  })

  test(`${name}: parameter editing preserves text, validates arguments and recovers commits after refresh`, async () => {
    const context = await browser.launchPersistentContext(resolve(temporary, `${name}-parameters`), { headless: true })
    try {
      const page = await context.newPage()
      await ready(page)
      assert.equal(await page.locator('[data-badge]:visible').filter({ hasText: /^0\.4\.0$/ }).isVisible(), true)
      const text = "O'Reilly'); DELETE FROM notes; --\0🦊"
      await execute(page, 'INSERT INTO notes (id, body) VALUES ($1, $2)', false, [1, text])
      assert.deepEqual((await execute(page, 'SELECT body FROM notes WHERE id = ?1', false, [1])).rows, [[text]])
      assert.match(await execute(page, 'SELECT body FROM notes WHERE id = $1', true), /INVALID_PARAMS/)
      assert.match(await execute(page, 'SELECT body FROM notes WHERE id = $1', true, [{}]), /INVALID_ARGUMENT/)
      await page.locator('#parameters').fill('[invalid JSON]')
      await page.getByRole('button', { name: 'Executar consulta' }).click()
      await page.waitForFunction(() => !document.querySelector('#execute').disabled)
      assert.match(await page.locator('#error').textContent(), /INVALID_ARGUMENT.*array JSON válido/)
      await transaction(page, 'BEGIN')
      await execute(page, 'UPDATE notes SET body = $1 WHERE id = $2', false, ['Pendente', 1])
      await page.reload()
      await page.waitForFunction(() => !document.querySelector('#execute').disabled)
      await page.getByRole('tab', { name: 'JSON', exact: true }).click()
      assert.deepEqual((await response(page)).rows, [[1, text]])
    } finally { await context.close() }
  })
}

for (const [name, browser] of Object.entries({ chromium, firefox })) {
  test(`${name}: downloaded backups restore to new files and survive offline refresh`, async () => {
    const context = await browser.launchPersistentContext(resolve(temporary, `${name}-backup`), { headless: true })
    try {
      const page = await context.newPage()
      await ready(page)
      await preset(page, 'Inserir')
      await transaction(page, 'BEGIN')
      await execute(page, 'UPDATE notes SET body = $1 WHERE id = $2', false, ['Uncommitted', 1])
      const [download] = await Promise.all([
        page.waitForEvent('download'),
        page.getByRole('button', { name: 'Baixar backup .rdb', exact: true }).click(),
      ])
      assert.equal(download.suggestedFilename(), 'reddb-browser-sql-playground-v1.rdb')
      const backup = await readFile(await download.path())
      assert.ok(backup.length > 0)
      await page.waitForFunction(() => !document.querySelector('#execute').disabled)
      assert.deepEqual((await preset(page, 'Ler')).rows, [[1, 'Uncommitted']])
      await page.getByLabel('Arquivo de backup .rdb', { exact: true }).setInputFiles({ name: 'invalid.rdb', mimeType: 'application/octet-stream', buffer: Buffer.alloc(32) })
      await page.getByRole('button', { name: 'Restaurar em novo banco', exact: true }).click()
      await page.waitForFunction(() => !document.querySelector('#execute').disabled)
      assert.match(await page.locator('#error').textContent(), /DATABASE_ERROR/)
      assert.equal(new URL(page.url()).searchParams.has('database'), false)
      assert.deepEqual((await preset(page, 'Ler')).rows, [[1, 'Uncommitted']])
      await page.getByLabel('Arquivo de backup .rdb', { exact: true }).setInputFiles({ name: 'notes.rdb', mimeType: 'application/octet-stream', buffer: backup })
      await page.getByRole('button', { name: 'Restaurar em novo banco', exact: true }).click()
      await page.waitForFunction(() => document.querySelector('#storage-status').textContent.includes('Backup restaurado'))
      await page.waitForFunction(() => !document.querySelector('#execute').disabled)
      assert.match(new URL(page.url()).searchParams.get('database'), /^reddb-browser-restored-/)
      assert.equal(await page.locator('#restore-backup').isDisabled(), true)
      assert.deepEqual((await response(page)).rows, [[1, 'Minha nota offline']])
      await context.setOffline(true)
      await page.reload()
      await page.waitForFunction(() => document.querySelector('#execute')?.disabled === false)
      await page.getByRole('tab', { name: 'JSON', exact: true }).click()
      assert.deepEqual((await response(page)).rows, [[1, 'Minha nota offline']])
      await page.getByRole('link', { name: 'Abrir banco original do playground', exact: true }).click()
      await page.waitForFunction(() => document.querySelector('#execute')?.disabled === false)
      await page.getByRole('tab', { name: 'JSON', exact: true }).click()
      assert.equal(new URL(page.url()).searchParams.has('database'), false)
      assert.deepEqual((await response(page)).rows, [[1, 'Minha nota offline']])
    } finally { await context.close() }
  })
}

test('a backup can recover a corrupt default database without overwriting its file', async () => {
  const browser = await chromium.launch({ headless: true })
  try {
    let context = await browser.newContext()
    let page = await context.newPage()
    await ready(page)
    await preset(page, 'Inserir')
    const [download] = await Promise.all([
      page.waitForEvent('download'),
      page.getByRole('button', { name: 'Baixar backup .rdb', exact: true }).click(),
    ])
    const backup = await readFile(await download.path())
    await context.close()
    context = await browser.newContext()
    page = await context.newPage()
    await page.route(origin, route => route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Recovery setup</title>' }))
    await page.goto(origin)
    await page.evaluate(async () => {
      const directory = await (await navigator.storage.getDirectory()).getDirectoryHandle('reddb-browser', { create: true })
      const file = await directory.getFileHandle('reddb-browser-sql-playground-v1.rdb', { create: true })
      const writer = await file.createWritable()
      await writer.write(new Uint8Array(32))
      await writer.close()
    })
    await page.unroute(origin)
    await page.goto(origin)
    await page.waitForFunction(() => document.querySelector('#error')?.textContent.includes('DATABASE_ERROR'))
    await page.getByLabel('Arquivo de backup .rdb', { exact: true }).setInputFiles({ name: 'recovery.rdb', mimeType: 'application/octet-stream', buffer: backup })
    assert.equal(await page.locator('#restore-backup').isEnabled(), true)
    await page.getByRole('button', { name: 'Restaurar em novo banco', exact: true }).click()
    await page.getByRole('gridcell', { name: 'Minha nota offline', exact: true }).waitFor()
    assert.equal(await page.locator('#error').isVisible(), false)
    const original = await page.evaluate(async () => {
      const directory = await (await navigator.storage.getDirectory()).getDirectoryHandle('reddb-browser')
      return [...new Uint8Array(await (await (await directory.getFileHandle('reddb-browser-sql-playground-v1.rdb')).getFile()).arrayBuffer())]
    })
    assert.deepEqual(original, Array(32).fill(0))
  } finally { await browser.close() }
})

test('storage retention reports the browser decision and origin quota without promising a grant', async () => {
  const browser = await chromium.launch({ headless: true })
  try {
    for (const grant of [false, true]) {
      const context = await browser.newContext()
      await context.addInitScript(grant => {
        let persisted = false
        Object.defineProperties(navigator.storage, {
          persisted: { value: async () => persisted },
          persist: { value: async () => { persisted = grant; return grant } },
          estimate: { value: async () => ({ usage: 1048576, quota: 104857600 }) },
        })
      }, grant)
      try {
        const page = await context.newPage()
        await ready(page)
        await page.locator('#storage-estimate').waitFor()
        assert.match(await page.locator('#storage-estimate').textContent(), /desta origem: 1 MB de 100 MB/)
        await page.getByRole('button', { name: 'Solicitar armazenamento persistente' }).click()
        await page.waitForFunction(() => !document.querySelector('#execute').disabled)
        assert.match(await page.locator('#storage-retention').textContent(), grant ? /persistente concedido/ : /não concedeu/)
        assert.equal(await page.locator('#persist-storage').isDisabled(), grant)
        await preset(page, 'Inserir')
        assert.deepEqual((await preset(page, 'Ler')).rows, [[1, 'Minha nota offline']])
      } finally { await context.close() }
    }
  } finally { await browser.close() }
})

test('unsupported storage remains explicit and does not enable write controls', async () => {
  const browser = await chromium.launch({ headless: true })
  try {
    const context = await browser.newContext()
    await context.addInitScript(() => Object.defineProperty(navigator.storage, 'getDirectory', { value: undefined }))
    const page = await context.newPage()
    await page.goto(origin)
    await page.waitForFunction(() => !document.querySelector('#error').hidden)
    assert.match(await page.locator('#error').textContent(), /STORAGE_UNSUPPORTED/)
    assert.equal(await page.getByRole('button', { name: 'BEGIN', exact: true }).isDisabled(), true)
    assert.equal(await page.getByRole('button', { name: 'Executar consulta' }).isDisabled(), true)
    assert.equal(await page.locator('#export-backup').isDisabled(), true)
    await page.getByLabel('Arquivo de backup .rdb', { exact: true }).setInputFiles({ name: 'test.rdb', mimeType: 'application/octet-stream', buffer: Buffer.alloc(32) })
    assert.equal(await page.locator('#restore-backup').isDisabled(), true)
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


test('query workspace follows the DS reference with a code editor, real result grid and keyboard execution', async () => {
  const browser = await chromium.launch({ headless: true })
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } })
    const page = await context.newPage()
    await ready(page)
    assert.equal(await page.locator('main').count(), 1)
    assert.equal(await page.getByRole('heading', { level: 1 }).count(), 1)
    assert.equal(await page.locator('html').getAttribute('data-density'), 'compact')
    // ready() has focused the result tab; check the bypass explicitly.
    await page.locator('a[href="#main-content"]').focus()
    await page.keyboard.press('Enter')
    assert.equal(await page.evaluate(() => document.activeElement.id), 'main-content')
    await page.getByRole('tab', { name: 'Resultado', exact: true }).click()
    assert.equal(await page.getByRole('columnheader', { name: 'id', exact: true }).isVisible(), true)
    assert.equal(await page.locator('[data-code-field]').count(), 1)
    assert.ok(await page.locator('[data-code-field] [data-token-kind="keyword"]').count() > 0)
    const editor = await page.locator('#sql').boundingBox()
    const results = await page.locator('#query-result').boundingBox()
    const file = await page.getByRole('complementary').boundingBox()
    assert.ok(editor.y < 160, 'query editing starts near the top of the application')
    assert.ok(results.y >= editor.y + editor.height, 'the result is directly below the editor')
    assert.ok(file.x + file.width <= editor.x, 'the local artifact panel sits on the leading side')
    const status = await page.locator('#engine-state').boundingBox()
    assert.ok(status.y + status.height <= 1000, 'the runtime status stays within the desktop viewport')
    await page.getByRole('tab', { name: 'JSON', exact: true }).click()
    await preset(page, 'Inserir')
    await page.getByRole('tab', { name: 'Resultado', exact: true }).click()
    await page.locator('#parameters').fill('[]')
    await page.getByRole('textbox', { name: 'Consulta RQL' }).fill("SELECT 'sem rede' AS modo, 42 AS resposta FROM notes")
    await page.keyboard.press('Control+Enter')
    await page.getByRole('gridcell', { name: 'sem rede', exact: true }).waitFor()
    assert.equal(await page.getByRole('columnheader', { name: 'modo', exact: true }).isVisible(), true)
    await page.getByRole('textbox', { name: 'Consulta RQL' }).fill('x'.repeat(10001))
    assert.equal(await page.getByRole('button', { name: 'Executar consulta' }).isDisabled(), true)
    await page.keyboard.press('Control+Enter')
    await page.waitForFunction(() => document.querySelector('#error').textContent.includes('10.000'))
    await page.getByRole('textbox', { name: 'Consulta RQL' }).fill("SELECT 'sem rede' AS modo, 42 AS resposta FROM notes")
    await page.keyboard.press('Control+Enter')
    await page.getByRole('gridcell', { name: 'sem rede', exact: true }).waitFor()
    await page.getByRole('switch', { name: 'Tema escuro' }).click()
    assert.equal(await page.locator('html').getAttribute('data-color-scheme'), 'light')
    await page.setViewportSize({ width: 390, height: 844 })
    const mobileEditor = await page.locator('#sql').boundingBox()
    const mobileResults = await page.locator('#query-result').boundingBox()
    const mobileFile = await page.getByRole('complementary').boundingBox()
    assert.ok(mobileResults.y >= mobileEditor.y + mobileEditor.height)
    assert.ok(mobileFile.y >= mobileResults.y + mobileResults.height, 'local file controls follow the query on phones')
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)
    await page.getByRole('button', { name: 'Menu', exact: true }).click()
    assert.equal(await page.getByRole('switch', { name: 'Tema escuro' }).isChecked(), false)
    assert.equal(await page.getByRole('link', { name: 'Guia JS/TS', exact: true }).isVisible(), true)
    await page.keyboard.press('Escape')
    assert.equal(await page.getByRole('button', { name: 'Menu', exact: true }).getAttribute('aria-expanded'), 'false')
    await page.getByRole('button', { name: 'Como testar offline', exact: true }).click()
    assert.equal(await page.getByText('Desligue a rede do navegador.', { exact: true }).isVisible(), true)
    await page.setViewportSize({ width: 1440, height: 1000 })
    await page.locator('[data-application-shell-region="main"]').evaluate(element => { element.style.width = '640px' })
    const containedResults = await page.locator('#query-result').boundingBox()
    const containedFile = await page.getByRole('complementary').boundingBox()
    assert.ok(containedFile.y >= containedResults.y + containedResults.height, 'the layout follows its own container on a wide viewport')
  } finally { await browser.close() }
})


test('an installed update waits for explicit activation, retains OPFS and supports a versioned URL offline', { skip: !!process.env.REDDB_BROWSER_EXAMPLE_URL }, async () => {
  const browser = await chromium.launch({ headless: true })
  try {
    const context = await browser.newContext()
    const page = await context.newPage()
    await ready(page)
    await preset(page, 'Inserir')
    await transaction(page, 'BEGIN')
    await execute(page, "INSERT INTO notes VALUES (2, 'Descartar ao atualizar')")
    updateRevision = true
    await page.evaluate(async () => (await navigator.serviceWorker.getRegistration()).update())
    await page.locator('#app-update').waitFor({ state: 'visible' })
    assert.equal(await page.evaluate(async () => (await navigator.serviceWorker.getRegistration()).waiting?.state), 'installed')
    assert.equal(await page.getByRole('button', { name: 'Recarregar página' }).isDisabled(), true)
    await Promise.all([
      page.waitForEvent('load'),
      page.getByRole('button', { name: 'Atualizar aplicação' }).click(),
    ])
    await page.getByRole('gridcell', { name: 'Minha nota offline', exact: true }).waitFor()
    await page.getByRole('tab', { name: 'JSON', exact: true }).click()
    await page.waitForFunction(() => !document.querySelector('#reload').disabled)
    assert.deepEqual((await response(page)).rows, [[1, 'Minha nota offline']])
    assert.equal(await page.locator('#app-update').isHidden(), true)
    const port = server.address().port
    await new Promise(resolve => server.close(resolve))
    const before = requests
    try {
      await context.setOffline(true)
      await page.goto(`${origin}?v=update-test`)
      await page.getByRole('gridcell', { name: 'Minha nota offline', exact: true }).waitFor()
      await page.getByRole('tab', { name: 'JSON', exact: true }).click()
      await page.waitForFunction(() => !document.querySelector('#reload').disabled)
      assert.deepEqual((await response(page)).rows, [[1, 'Minha nota offline']])
      await execute(page, "UPDATE notes SET body = 'Atualizada offline' WHERE id = 1")
      assert.deepEqual((await preset(page, 'Ler')).rows, [[1, 'Atualizada offline']])
      assert.equal(requests, before, 'the versioned navigation must use the offline cache')
    } finally {
      server.listen(port, '127.0.0.1')
      await once(server, 'listening')
    }
  } finally { updateRevision = false; await browser.close() }
})

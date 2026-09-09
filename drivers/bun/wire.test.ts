import { strict as assert } from 'node:assert'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createServer } from 'node:net'
import { connect } from './index'

// Exercise this package's TCP client against a real server. An absent binary
// is a test failure: silently skipping hid the unbound-query regression.
const binary = process.env.REDDB_BINARY_PATH || resolve(import.meta.dir, '../../target/debug/red')
const reservation = createServer()
await new Promise<void>((resolve, reject) => {
  reservation.once('error', reject)
  reservation.listen(0, '127.0.0.1', resolve)
})
const address = reservation.address()
assert(address && typeof address === 'object')
const endpoint = `127.0.0.1:${address.port}`
await new Promise<void>((resolve) => reservation.close(() => resolve()))
const directory = mkdtempSync(join(tmpdir(), 'reddb-bun-wire-'))
const server = Bun.spawn([binary, 'server', '--path', join(directory, 'db.rdb'),
  '--bind', endpoint, '--no-auth', '--no-log-file'], { stdout: 'ignore', stderr: 'inherit' })
let db: Awaited<ReturnType<typeof connect>> | undefined
try {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    if (server.exitCode !== null) throw new Error(`server exited ${server.exitCode}`)
    try {
      const response = await fetch(`http://${endpoint}/health`, { signal: AbortSignal.timeout(500) })
      if (response.ok) break
    } catch {}
    await Bun.sleep(100)
  }
  db = await connect(endpoint)
  assert(db.supportsParams())
  await db.query('CREATE TABLE wire_rows (id INTEGER, name TEXT)')
  const inserted = await db.queryParsed("INSERT INTO wire_rows (id,name) VALUES (1,'Bun')")
  assert.equal(inserted.affected_rows, 1)
  const unbound = await db.queryParsed('SELECT id,name FROM wire_rows')
  const bound = await db.queryParsed('SELECT id,name FROM wire_rows WHERE id=$1', [1])
  assert.deepEqual(unbound.result.records.map((record: any) => record.values), [{ id: 1, name: 'Bun' }])
  assert.deepEqual(unbound.result.records.map((record: any) => record.values), bound.result.records.map((record: any) => record.values))
  assert.deepEqual((await db.queryParsed('SELECT id,name FROM wire_rows', [])).result.records.map((record: any) => record.values), bound.result.records.map((record: any) => record.values))
  assert.deepEqual((await db.queryParsed('SELECT id FROM wire_rows WHERE id=2')).result.records, [])
  await assert.rejects(db.query('SELECT * FROM absent_wire_table'))
  const cli = Bun.spawnSync([binary, 'query', '--bind', endpoint, '--json',
    "INSERT INTO wire_rows (id,name) VALUES (2,'CLI')"])
  assert.equal(cli.exitCode, 0, cli.stderr.toString())
  const insertedByCli = JSON.parse(cli.stdout.toString()).data
  assert.equal(insertedByCli.affected, 1)
  assert.equal(insertedByCli.statement, 'insert')
  console.log('ok Bun wire bound/unbound result contract')
} finally {
  db?.close()
  server.kill()
  await server.exited
  rmSync(directory, { recursive: true, force: true })
}

import assert from 'node:assert/strict'
import { after, test } from 'node:test'
import { createRuntime, createInteropRuntime, BrowserError } from '../src/index.js'

const info = {
  abiVersion: 1,
  valueEncoding: 'reddb-json-v1',
  capabilities: { queryValidation: true, scalarQueries: true, storedQueries: false, transactions: false, sync: false },
}
class ScriptedWorker {
  messages = []
  terminated = 0
  responseVersion = 1
  info = info
  defer = false
  postMessage(request) {
    this.messages.push(request)
    if (this.defer) return
    queueMicrotask(() => {
      const result = request.method === 'initialize' || request.method === 'info'
        ? this.info : { columns: ['value'], rows: [[{ $int: '9007199254740993' }]] }
      this.onmessage?.({ data: { abiVersion: this.responseVersion, id: request.id, result: structuredClone(result) } })
    })
  }
  terminate() { this.terminated++ }
}
const originalWorker = Object.getOwnPropertyDescriptor(globalThis, 'Worker')
Object.defineProperty(globalThis, 'Worker', { value: ScriptedWorker, configurable: true })
after(() => {
  if (originalWorker) Object.defineProperty(globalThis, 'Worker', originalWorker)
  else delete globalThis.Worker
})

test('a runtime initializes without OPFS and advertises only supported operations', async () => {
  const worker = new ScriptedWorker()
  const runtime = await createRuntime({ worker })
  assert.deepEqual(await runtime.info(), info)
  assert.equal(worker.messages.some(request => request.method === 'open'), false)
  assert.deepEqual(await runtime.executeScalar('SELECT 9007199254740993'), { columns: ['value'], rows: [[{ $int: '9007199254740993' }]] })
  await runtime.close()
  await runtime.close()
  assert.equal(worker.terminated, 1)
  await assert.rejects(runtime.info(), { code: 'CLOSED' })
})

test('worker and WASM ABI mismatches fail before opening storage', async () => {
  for (const mismatch of ['worker', 'wasm', 'encoding', 'capability']) {
    const worker = new ScriptedWorker()
    if (mismatch === 'worker') worker.responseVersion = undefined
    else if (mismatch === 'wasm') worker.info = { ...info, abiVersion: 2 }
    else if (mismatch === 'encoding') worker.info = { ...info, valueEncoding: 'unknown' }
    else worker.info = { ...info, capabilities: { ...info.capabilities, scalarQueries: false } }
    await assert.rejects(createRuntime({ worker }), { code: 'VERSION_MISMATCH' })
    assert.equal(worker.terminated, 1)
    assert.deepEqual(worker.messages.map(request => request.method), ['initialize'])
  }
})

test('close rejects pending calls without waiting for a worker reply', async () => {
  const worker = new ScriptedWorker()
  const runtime = await createRuntime({ worker })
  worker.defer = true
  const pending = assert.rejects(runtime.executeScalar('SELECT 1'), { code: 'CLOSED' })
  await runtime.close()
  await pending
  assert.equal(worker.terminated, 1)
})

test('worker failures reject pending and future runtime calls', async () => {
  const worker = new ScriptedWorker()
  const runtime = await createRuntime({ worker })
  worker.defer = true
  const pending = assert.rejects(runtime.info(), { code: 'WORKER_FAILED' })
  worker.onerror({ message: 'worker stopped' })
  await pending
  await assert.rejects(runtime.info(), { code: 'CLOSED' })
  assert.equal(worker.terminated, 1)
})

test('the interop boundary preserves exact tags and structured errors', async () => {
  const worker = new ScriptedWorker()
  const runtime = createInteropRuntime({ worker })
  assert.deepEqual(await runtime.invoke('info'), { ok: true, result: info })
  assert.deepEqual(await runtime.invoke('executeScalar', { sql: 'SELECT 9007199254740993' }), {
    ok: true, result: { columns: ['value'], rows: [[{ $int: '9007199254740993' }]] },
  })
  assert.equal((await runtime.invoke('unknown')).error.code, 'INVALID_ARGUMENT')
  assert.equal((await runtime.invoke('executeScalar', null)).error.code, 'INVALID_ARGUMENT')
  assert.equal((await runtime.invoke('executeScalar', { sql: 1 })).error.code, 'INVALID_ARGUMENT')
  runtime.close()
  runtime.close()
  assert.equal((await runtime.invoke('info')).error.code, 'CLOSED')
  assert.equal(worker.terminated, 1)
})

test('an interop bridge can close during initialization and report startup failures', async () => {
  const worker = new ScriptedWorker()
  worker.defer = true
  const runtime = createInteropRuntime({ worker })
  runtime.close()
  assert.equal((await runtime.invoke('info')).error.code, 'CLOSED')
  assert.equal(worker.terminated, 1)
  const invalid = createInteropRuntime({ worker: {} })
  const failure = await invalid.invoke('info')
  assert.equal(failure.ok, false)
  assert.equal(failure.error.code, 'INVALID_ARGUMENT')
  invalid.close()
})

test('public runtime validates SQL arguments before cloning them to a worker', async () => {
  const worker = new ScriptedWorker()
  const runtime = await createRuntime({ worker })
  await assert.rejects(runtime.executeScalar(() => {}), BrowserError)
  await assert.rejects(runtime.validateQuery(null), { code: 'INVALID_ARGUMENT' })
  assert.deepEqual(worker.messages.map(request => request.method), ['initialize'])
  await runtime.close()
})

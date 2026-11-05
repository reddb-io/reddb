import test from 'node:test'
import { execFileSync } from 'node:child_process'

test('a pending RedWire query does not retain the caller backing buffer after encoding', () => {
  execFileSync(process.execPath, ['--expose-gc', '--input-type=module', '-e', `
    import assert from 'node:assert/strict'
    import { setImmediate as nextTurn } from 'node:timers/promises'
    import { RedDB } from './src/core/reddb.js'
    import { RedWireClient, Features, MessageKind } from './src/redwire-core.js'

    let finish
    const client = new RedWireClient(
      { write(_, done) { done() } },
      { next() { return new Promise(resolve => { finish = resolve }) } },
      '', Features.PARAMS,
    )
    const db = new RedDB(client)
    function start() {
      const buffer = new ArrayBuffer(1024 * 1024)
      const view = new Float32Array(buffer, 0, 1536)
      return { weak: new WeakRef(buffer), pending: db.query('SELECT $1', view) }
    }
    const { weak, pending } = start()
    for (let i = 0; i < 4; i++) { await nextTurn(); globalThis.gc() }
    assert.equal(weak.deref(), undefined, 'caller backing buffer retained during network wait')
    finish({ kind: MessageKind.Result, payload: new TextEncoder().encode('{"ok":true,"columns":[],"rows":[]}') })
    await pending
  `], { cwd: new URL('..', import.meta.url), timeout: 10000 })
})

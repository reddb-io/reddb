// Fase 3: verificação de convergência de replicação de ponta a ponta.
// Escreve um marker único no primary e lê de volta na replica.
import { connectRedwire } from '../../drivers/js-client/src/redwire.js'

const marker = 'phase3-' + Date.now()
const payload = JSON.stringify({ marker })

const w = await connectRedwire({ host: '127.0.0.1', port: 5050, clientName: 'phase3-write' })
await w.call('query', { sql: `KV PUT 'repl:phase3' = '${payload}'` })
w.socket.end()

const r = await connectRedwire({ host: '127.0.0.1', port: 5051, clientName: 'phase3-read' })
const got = await r.call('query', { sql: `KV GET 'repl:phase3'` })
const raw = got.rows?.[0]?.value
const v = raw ? JSON.parse(raw) : {}
console.log('escrito no primary :', marker)
console.log('lido na replica    :', v.marker ?? '(vazio)')
console.log('convergência       :', v.marker === marker ? 'OK' : 'DIVERGIU')
r.socket.end()

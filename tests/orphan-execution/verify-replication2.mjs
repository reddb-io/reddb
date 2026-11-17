// Verifica convergência com retries (lag esperado)
import { connectRedwire } from '../../drivers/js-client/src/redwire.js'
const marker = 'phase3-' + Date.now()
const payload = JSON.stringify({ marker })
const w = await connectRedwire({ host: '127.0.0.1', port: 5050, clientName: 'p3-write' })
await w.call('query', { sql: `KV PUT 'repl:phase3' = '${payload}'` })
w.socket.end()
let readMarker = null
for (let i = 0; i < 15; i++) {
  await new Promise(r => setTimeout(r, 2000))
  const r = await connectRedwire({ host: '127.0.0.1', port: 5051, clientName: 'p3-read' })
  const got = await r.call('query', { sql: `KV GET 'repl:phase3'` })
  const raw = got.rows?.[0]?.value
  r.socket.end()
  if (raw) { readMarker = JSON.parse(raw).marker; if (readMarker === marker) break }
}
console.log('escrito no primary :', marker)
console.log('lido na replica    :', readMarker ?? '(nunca convergiu)')
console.log('convergência       :', readMarker === marker ? 'OK' : 'DIVERGIU')

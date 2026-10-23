import init, { runConformance, entityTimestamps } from './wasm/conformance.js'

try {
  await init()
  const before = BigInt(Math.floor(Date.now() / 1000))
  const timestamps = entityTimestamps()
  const after = BigInt(Math.floor(Date.now() / 1000))
  const earliest = before < after ? before : after
  const latest = before > after ? before : after
  if (!timestamps.every((stamp) => stamp >= earliest && stamp <= latest)) {
    throw new Error('WASM entity timestamps must match host Unix seconds')
  }
  self.postMessage({ passed: runConformance().split(',') })
} catch (failure) {
  self.postMessage({ error: failure.message || String(failure) })
}

import init, { runConformance, entityTimestamps, runtimeClockValues } from './wasm/conformance.js'

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
  const clockBefore = BigInt(Date.now())
  const clocks = runtimeClockValues()
  const clockAfter = BigInt(Date.now())
  const first = clockBefore < clockAfter ? clockBefore : clockAfter
  const last = clockBefore > clockAfter ? clockBefore : clockAfter
  const day = 86400000n
  if (!clocks.slice(0, 2).every((value) => value >= first && value <= last)
      || clocks[2] < first / day || clocks[2] > last / day) {
    throw new Error('WASM clock scalars must match host Unix milliseconds and UTC date')
  }
  const firstTime = first % day
  const lastTime = last % day
  const timeMatches = firstTime <= lastTime
    ? clocks[3] >= firstTime && clocks[3] <= lastTime
    : clocks[3] >= firstTime || clocks[3] <= lastTime
  if (!timeMatches) throw new Error('WASM CURRENT_TIME must match host UTC time')
  self.postMessage({ passed: runConformance().split(',') })
} catch (failure) {
  self.postMessage({ error: failure.message || String(failure) })
}

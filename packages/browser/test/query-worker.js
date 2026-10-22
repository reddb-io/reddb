import init, { runConformance } from './wasm/conformance.js'

try {
  await init()
  self.postMessage({ passed: runConformance().split(',') })
} catch (failure) {
  self.postMessage({ error: failure.message || String(failure) })
}

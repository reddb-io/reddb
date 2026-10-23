// Test-only recovery probe; the production SDK does not expose this decoder.
import init, { verifyNativeSnapshot } from './wasm/conformance.js'

self.onmessage = async ({ data: snapshot }) => {
  try {
    await init()
    self.postMessage({ entities: verifyNativeSnapshot(snapshot) })
  } catch (failure) {
    self.postMessage({ error: failure.message || String(failure) })
  }
}

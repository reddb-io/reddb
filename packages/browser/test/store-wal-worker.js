// Test-only logical recovery. The public SDK still exposes artifact storage.
import init, { storeWalFixtureCount, storeWalFixtureAction, StoreWalRecoveryFixture } from './wasm/conformance.js'

self.onmessage = async ({ data: payloads }) => {
  try {
    await init()
    if (payloads === null) {
      self.postMessage({ actions: Array.from({ length: storeWalFixtureCount() }, (_, index) => storeWalFixtureAction(index)) })
      return
    }
    const store = new StoreWalRecoveryFixture()
    try {
      for (const payload of payloads) store.replay(payload)
      self.postMessage({ entities: store.verify() })
    } finally { store.free() }
  } catch (failure) {
    self.postMessage({ error: failure.message || String(failure) })
  }
}

// Test-only catalog reconstruction; public stored-data queries remain pending.
import init, { catalogSnapshotFixture, verifyCatalogSnapshot } from './wasm/conformance.js'

self.onmessage = async ({ data }) => {
  try {
    await init()
    const snapshot = data || catalogSnapshotFixture()
    self.postMessage({ contracts: verifyCatalogSnapshot(snapshot), snapshot })
  } catch (failure) {
    self.postMessage({ error: failure.message || String(failure) })
  }
}

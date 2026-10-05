/**
 * In-process embedded engine (the `reddb.node` Node addon).
 *
 * Embedded mode used to spawn `red rpc --stdio` and speak JSON-RPC over the
 * child's pipes. The addon serves the same protocol — same methods, same
 * transaction/cursor semantics — as a function call, so there is no
 * subprocess and no separate `red` binary involved.
 *
 * The addon ships inside the `@reddb-io/red-<platform>` package next to
 * `bin/red` (see `platform-binary.js` for how that package is found).
 * `REDDB_NATIVE_ADDON=/path/to/reddb.node` overrides the lookup.
 */

import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'

import { RedDBError } from './protocol.js'
import { platformPackageNames } from './platform-binary.js'

const require = createRequire(import.meta.url)

/**
 * @returns {string | null} path to `reddb.node`, or null when no addon is
 *   installed for this host.
 */
export function resolveNativeAddon({
  resolve = (id) => require.resolve(id),
  env = process.env,
  names = platformPackageNames(),
} = {}) {
  const override = env.REDDB_NATIVE_ADDON
  if (typeof override === 'string' && override !== '') return override
  for (const name of names) {
    let manifest
    try {
      manifest = resolve(`${name}/package.json`)
    } catch {
      continue
    }
    const addon = join(dirname(manifest), 'reddb.node')
    if (existsSync(addon)) return addon
  }
  return null
}

/**
 * Open an embedded engine through the addon.
 * @param {string | undefined} path database file, or undefined for in-memory
 * @param {string} addonPath
 */
export function openNativeEngine(path, addonPath) {
  const { Engine } = require(addonPath)
  return Engine.open(path)
}

/**
 * JSON-RPC client over an in-process engine. Same surface as `RpcClient`
 * (`call`, `close`), so `RedDB` cannot tell the transports apart.
 *
 * Requests are chained so they reach the engine in call order even when the
 * caller does not await each one: the engine's thread pool would otherwise
 * be free to reorder them, which a transaction (`tx.begin`, then writes)
 * must never see. The stdio pipe gave this ordering for free.
 */
export class NativeRpcClient {
  /** @param {{ call(request: string): Promise<string>, close(): void }} engine */
  constructor(engine) {
    this.engine = engine
    this.nextId = 1
    this.closed = false
    this.closeReason = null
    this.tail = Promise.resolve()
  }

  call(method, params = {}) {
    if (this.closed) {
      return Promise.reject(
        new RedDBError('CLIENT_CLOSED', `client is closed: ${this.closeReason ?? 'unknown'}`),
      )
    }
    const id = this.nextId++
    const request = JSON.stringify({ jsonrpc: '2.0', id, method, params })
    const run = this.tail.then(() => this.engine.call(request))
    this.tail = run.then(
      () => undefined,
      () => undefined,
    )
    return run.then((line) => {
      const envelope = JSON.parse(line)
      if (method === 'close') this.#shutdown('close requested')
      if (envelope.error) {
        throw new RedDBError(
          envelope.error.code ?? 'UNKNOWN',
          envelope.error.message ?? 'unknown error',
          envelope.error.data,
        )
      }
      return envelope.result
    })
  }

  /** Send `close` (discarding any open transaction), then release the file lock. */
  async close() {
    if (this.closed) return
    try {
      await this.call('close', {})
    } catch {
      // best effort — the engine may already be shut down
    }
    this.#shutdown('close requested')
    this.engine.close()
  }

  #shutdown(reason) {
    if (this.closed) return
    this.closed = true
    this.closeReason = reason
  }
}

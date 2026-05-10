import { RedDBError } from './protocol.js'

export class KvClient {
  constructor(client, collection = 'kv_default') {
    this.client = client
    this.collection = collection
  }

  async *watch(key, options = {}) {
    if (!this.client.baseUrl) {
      throw new RedDBError('UNSUPPORTED_TRANSPORT', 'kv.watch requires the HTTP transport')
    }
    const collection = options.collection ?? this.collection
    const params = new URLSearchParams()
    if (options.sinceLsn != null) params.set('since_lsn', String(options.sinceLsn))
    if (options.limit != null) params.set('limit', String(options.limit))
    const suffix = params.toString() ? `?${params}` : ''
    const url = `${this.client.baseUrl}/collections/${encodeURIComponent(collection)}/kv/${encodeURIComponent(String(key))}/watch${suffix}`
    const response = await fetch(url, this.client.attachAuth({ method: 'GET' }))
    if (!response.ok) {
      throw new RedDBError('HTTP_ERROR', `kv.watch failed with HTTP ${response.status}`)
    }
    const text = await response.text()
    for (const block of text.split('\n\n')) {
      const line = block.split('\n').find((entry) => entry.startsWith('data: '))
      if (line) yield JSON.parse(line.slice(6))
    }
  }
}

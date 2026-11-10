/** Shared connection lifecycle for runtime-specific transports. */
export interface RoutingTransport {
  binaryQueryParams?: boolean
  call(method: string, params?: Record<string, unknown>): Promise<any>
  close(): Promise<void> | void
}
export interface RoutingReadOptions {
  consistency: 'strong' | 'local' | 'bounded_staleness'
  maxLagMs?: number
}
export class RoutingClient implements RoutingTransport {
  constructor(seed: string, client: RoutingTransport,
    dial: (address: string, signal: AbortSignal) => Promise<RoutingTransport>,
    protocol: string, options?: { forcePrimary?: boolean; now?: () => number })
  binaryQueryParams: boolean
  initialize(): Promise<this>
  call(method: string, params?: Record<string, unknown>): Promise<any>
  queryWithOptions(sql: string, options: RoutingReadOptions, params?: unknown[]): Promise<any>
  close(): Promise<void>
}

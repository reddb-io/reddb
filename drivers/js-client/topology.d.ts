export interface TopologyEndpoint { addr: string; region: string }
export interface TopologyReplica extends TopologyEndpoint {
  healthy: boolean
  lag_ms: number
  last_applied_lsn: string
  rebootstrapping: boolean
}
export interface Topology {
  version: 1
  epoch: string
  primary: TopologyEndpoint
  replicas: TopologyReplica[]
}
export function decodeTopology(bytes: Uint8Array): Topology | null
export function topologyAddress(address: unknown, protocol: string): string | null
export function isReplicaQuery(sql: string): boolean
export function transactionCommand(sql: string): string | null
export function isSessionQuery(sql: string): boolean
export const TOPOLOGY_TRANSPORT_METADATA_KEY: 'x-reddb-topology-transport'

export interface RedDBConnection {
  query(sql: string): Promise<string>
  queryParsed(sql: string): Promise<any>
  queryRaw(sql: string): Promise<number>
  bulkInsert(collection: string, jsonPayloads: string[]): Promise<number>
  close(): void
}

export function connect(addr: string): Promise<RedDBConnection>

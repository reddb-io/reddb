export interface ExecResult {
  code: number;
  stdout: string;
  stderr: string;
  args: string[];
}

export interface RedDBClientOptions {
  binaryPath?: string;
  targetDir?: string;
  autoDownload?: boolean;
  githubToken?: string;
  version?: string;
  cwd?: string;
  env?: Record<string, string | undefined>;
  timeout?: number;
}

export interface QueryResult {
  ok: boolean;
  mode?: string;
  statement?: string;
  record_count?: number;
  affected_rows?: number;
  result?: {
    columns: string[];
    records: Record<string, unknown>[];
  };
  [key: string]: unknown;
}

export interface EntityResult {
  ok: boolean;
  id: string;
  entity?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface HealthResult {
  healthy: boolean;
  state: string;
  [key: string]: unknown;
}

export interface RedDBClient {
  $binaryPath: string;
  server(flags?: Record<string, unknown>): Promise<unknown>;
  query(sql: string, flags?: Record<string, unknown>): Promise<QueryResult>;
  insert(collection: string, data: Record<string, unknown>, flags?: Record<string, unknown>): Promise<EntityResult>;
  get(collection: string, id: string, flags?: Record<string, unknown>): Promise<EntityResult>;
  delete(collection: string, id: string, flags?: Record<string, unknown>): Promise<EntityResult>;
  health(flags?: Record<string, unknown>): Promise<HealthResult>;
  auth(subcommand: string, flags?: Record<string, unknown>): Promise<unknown>;
  connect(addr?: string, flags?: Record<string, unknown>): unknown;
  mcp(flags?: Record<string, unknown>): unknown;
  version(): Promise<string>;
  exec(args: string[]): Promise<ExecResult>;
  spawn(args: string[], opts?: Record<string, unknown>): unknown;
}

export interface WrapperOptions extends RedDBClientOptions {
  binaryName?: string;
  channel?: 'stable' | 'latest' | 'next';
  force?: boolean;
  repo?: string;
  releaseVersion?: string;
  staticBuild?: boolean;
  verify?: boolean;
  source?: string;
  assetName?: string;
  maxBuffer?: number;
  stdio?: 'inherit' | 'pipe' | 'ignore' | string | (string | number | null)[];
}

export interface BinaryInfo {
  binaryPath: string;
  source?: string;
  version?: string | null;
}

export interface BinaryInstallResult extends BinaryInfo {
  source: string;
  changed: boolean;
  version?: string;
}

export interface BinaryUpgradeResult {
  binaryPath: string;
  previousVersion: string | null;
  version: string;
  changed: boolean;
  source?: string;
}

export interface WrapperStatus {
  binaryPath: string;
  currentVersion: string | null;
  latestVersion: string;
  source?: string | null;
  hasUpdate: boolean;
}

export interface ResolveOptions {
  cwd?: string;
  env?: Record<string, string | undefined>;
  timeout?: number;
  maxBuffer?: number;
  stdio?: 'inherit' | 'pipe' | 'ignore' | string | (string | number | null)[];
}

/** SDK version string (e.g. "0.1.0"), read from package.json at load time. */
export const version: string;

export function createClient(options?: RedDBClientOptions): Promise<RedDBClient>;
export function resolveBinary(options?: WrapperOptions): Promise<string>;
export function downloadBinary(options?: WrapperOptions): Promise<string>;
export function ensureInstalled(options?: WrapperOptions): Promise<BinaryInstallResult>;
export function getBinaryInfo(options?: WrapperOptions): Promise<BinaryInfo>;
export function getInstalledVersion(binaryPath: string, options?: ResolveOptions): Promise<string | null>;
export function checkForUpdates(options?: WrapperOptions): Promise<WrapperStatus>;
export function upgradeBinary(options?: WrapperOptions): Promise<BinaryUpgradeResult>;
export function resolveAssetName(options?: WrapperOptions): string;
export function resolveBinaryWithInfo(options?: WrapperOptions): Promise<{ binaryPath: string; source: string; version?: string }>;
export function execFile(binaryPath: string, args: string[], options?: ResolveOptions): Promise<ExecResult>;
export function runJson(binaryPath: string, args: string[], options?: ResolveOptions): Promise<unknown>;
export function spawnChild(binaryPath: string, args: string[], options?: ResolveOptions & { detached?: boolean }): unknown;
export function runCli(argv?: string[], runtime?: Record<string, unknown>): Promise<number>;

export interface InternalNamespace {
  buildFlags: (...args: unknown[]) => unknown;
  checkForUpdates: (...args: unknown[]) => unknown;
  defaultInstallDir: (...args: unknown[]) => unknown;
  downloadToFile: (...args: unknown[]) => unknown;
  ensureInstalled: (...args: unknown[]) => unknown;
  ensureObject: (...args: unknown[]) => unknown;
  execFilePromise: (...args: unknown[]) => unknown;
  exists: (...args: unknown[]) => unknown;
  formatWrapperBinaryStatus: (...args: unknown[]) => unknown;
  formatWrapperHelp: (...args: unknown[]) => unknown;
  getBinaryInfo: (...args: unknown[]) => unknown;
  getDefaultBinaryName: (...args: unknown[]) => unknown;
  getInstalledVersion: (...args: unknown[]) => unknown;
  getReleaseTag: (...args: unknown[]) => unknown;
  isExecutable: (...args: unknown[]) => unknown;
  kebabToCamel: (...args: unknown[]) => unknown;
  legacyInstallDir: (...args: unknown[]) => unknown;
  normalizeReleaseTag: (...args: unknown[]) => unknown;
  parseInstalledVersion: (...args: unknown[]) => unknown;
  parseWrapperArgs: (...args: unknown[]) => unknown;
  request: (...args: unknown[]) => unknown;
  requestJson: (...args: unknown[]) => unknown;
  requestText: (...args: unknown[]) => unknown;
  resolveFromPath: (...args: unknown[]) => unknown;
  resolveBinaryWithInfo: (...args: unknown[]) => unknown;
  resolveLegacyBinaryPath: (...args: unknown[]) => unknown;
  resolvePackageLocalBinaryPath: (...args: unknown[]) => unknown;
  resolveManagedBinaryPath: (...args: unknown[]) => unknown;
  resolveManagedUpgradeDestination: (...args: unknown[]) => unknown;
  sha256File: (...args: unknown[]) => unknown;
  splitWrapperArgs: (...args: unknown[]) => unknown;
  spawnBinary: (...args: unknown[]) => unknown;
  upgradeBinary: (...args: unknown[]) => unknown;
  waitForChild: (...args: unknown[]) => unknown;
  writeLine: (...args: unknown[]) => unknown;
  verifyChecksum: (...args: unknown[]) => unknown;
}

export interface RedDBSdkExports {
  version: string;
  checkForUpdates: typeof checkForUpdates;
  createClient: typeof createClient;
  downloadBinary: typeof downloadBinary;
  ensureInstalled: typeof ensureInstalled;
  getBinaryInfo: typeof getBinaryInfo;
  getInstalledVersion: typeof getInstalledVersion;
  runCli: typeof runCli;
  runJson: typeof runJson;
  resolveAssetName: typeof resolveAssetName;
  resolveBinary: typeof resolveBinary;
  resolveBinaryWithInfo: typeof resolveBinaryWithInfo;
  upgradeBinary: typeof upgradeBinary;
  execFile: typeof execFile;
  spawnChild: typeof spawnChild;
  _internal: InternalNamespace;
  default: RedDBSdkExports;
}

declare const reddbSdk: RedDBSdkExports;

export = reddbSdk;

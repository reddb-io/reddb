#!/usr/bin/env node
'use strict';

const crypto = require('crypto');
const fs = require('fs');
const fsp = fs.promises;
const https = require('https');
const os = require('os');
const path = require('path');
const { execFile, spawn } = require('child_process');

const SDK_VERSION = require(path.join(__dirname, '..', 'package.json')).version;
const DEFAULT_REPO = 'forattini-dev/reddb';

function getDefaultBinaryName(platform = process.platform) {
  return platform === 'win32' ? 'red.exe' : 'red';
}

const DEFAULT_BINARY_NAME = getDefaultBinaryName();

function kebabToCamel(value) {
  return String(value).replace(/[-_]+([a-zA-Z0-9])/g, (_, ch) => ch.toUpperCase());
}

function ensureObject(value, label) {
  if (value == null) {
    return {};
  }
  if (typeof value !== 'object' || Array.isArray(value)) {
    throw new TypeError(`${label} must be an object`);
  }
  return value;
}

function exists(filePath) {
  try {
    fs.accessSync(filePath, fs.constants.F_OK);
    return true;
  } catch (_) {
    return false;
  }
}

function isExecutable(filePath) {
  try {
    fs.accessSync(filePath, fs.constants.X_OK);
    return true;
  } catch (_) {
    return false;
  }
}

function resolveFromPath(binaryName) {
  const pathValue = process.env.PATH || '';
  for (const directory of pathValue.split(path.delimiter)) {
    if (!directory) {
      continue;
    }
    const candidate = path.join(directory, binaryName);
    if (exists(candidate) && (process.platform === 'win32' || isExecutable(candidate))) {
      return candidate;
    }
  }
  return null;
}

function defaultInstallDir() {
  return process.env.REDDB_INSTALL_DIR || process.env.INSTALL_DIR || path.join(os.homedir(), '.local', 'bin');
}

function legacyInstallDir() {
  return path.join(os.homedir(), '.reddb', 'bin');
}

function resolveManagedBinaryPath(options = {}) {
  if (options.binaryPath) {
    return path.resolve(options.binaryPath);
  }

  const installDir = options.targetDir || defaultInstallDir();
  const binaryName = options.binaryName || DEFAULT_BINARY_NAME;
  return path.resolve(installDir, binaryName);
}

function resolveLegacyBinaryPath(options = {}) {
  if (options.binaryPath || options.targetDir) {
    return null;
  }

  return path.resolve(legacyInstallDir(), options.binaryName || DEFAULT_BINARY_NAME);
}

function resolvePackageLocalBinaryPath(options = {}) {
  const binaryName = options.binaryName || DEFAULT_BINARY_NAME;
  const packageRoot = path.resolve(__dirname, '..');
  return path.resolve(packageRoot, '.reddb', 'bin', binaryName);
}

function resolveAssetName(options = {}) {
  const platform = options.platform || process.platform;
  const arch = options.arch || process.arch;
  const staticBuild = options.staticBuild === true;

  if (platform === 'linux' && arch === 'x64') {
    return 'red-linux-x86_64';
  }
  if (platform === 'linux' && arch === 'arm64') {
    return staticBuild ? 'red-linux-aarch64-static' : 'red-linux-aarch64';
  }
  if (platform === 'linux' && (arch === 'arm' || arch === 'armv7l')) {
    return 'red-linux-armv7';
  }
  if (platform === 'darwin' && arch === 'x64') {
    return 'red-macos-x86_64';
  }
  if (platform === 'darwin' && arch === 'arm64') {
    return 'red-macos-aarch64';
  }
  if (platform === 'win32' && arch === 'x64') {
    return 'red-windows-x86_64.exe';
  }

  throw new Error(`Unsupported reddb platform combination: ${platform}/${arch}`);
}

function request(url, options = {}) {
  return new Promise((resolve, reject) => {
    const headers = Object.assign(
      {
        'User-Agent': 'reddb-sdk',
        Accept: 'application/vnd.github+json'
      },
      options.headers || {}
    );

    const req = https.request(url, { headers }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        resolve(request(res.headers.location, options));
        return;
      }

      const chunks = [];
      res.on('data', (chunk) => chunks.push(chunk));
      res.on('end', () => {
        const body = Buffer.concat(chunks);
        if (res.statusCode < 200 || res.statusCode >= 300) {
          const error = new Error(
            `Request failed with status ${res.statusCode}: ${body.toString('utf8')}`
          );
          error.statusCode = res.statusCode;
          error.body = body.toString('utf8');
          reject(error);
          return;
        }
        resolve({ res, body });
      });
    });

    req.on('error', reject);
    req.end();
  });
}

async function requestJson(url, options = {}) {
  const { body } = await request(url, options);
  return JSON.parse(body.toString('utf8'));
}

async function requestText(url, options = {}) {
  const { body } = await request(url, options);
  return body.toString('utf8');
}

async function getReleaseTag(options = {}) {
  const repo = options.repo || DEFAULT_REPO;
  const githubToken =
    options.githubToken || (options.env && options.env.GITHUB_TOKEN) || process.env.GITHUB_TOKEN;
  const headers = githubToken ? { Authorization: `Bearer ${githubToken}` } : {};
  const requestedVersion = options.releaseVersion || options.version;

  if (requestedVersion) {
    return String(requestedVersion).startsWith('v')
      ? String(requestedVersion)
      : `v${requestedVersion}`;
  }

  const channel = options.channel || 'stable';

  if (channel === 'stable') {
    const release = await requestJson(`https://api.github.com/repos/${repo}/releases/latest`, {
      headers
    });
    return release.tag_name;
  }

  const releases = await requestJson(`https://api.github.com/repos/${repo}/releases`, {
    headers
  });

  if (!Array.isArray(releases) || releases.length === 0) {
    throw new Error(`No releases found for ${repo}`);
  }

  if (channel === 'next') {
    const prerelease = releases.find((release) => release && release.prerelease);
    if (prerelease) {
      return prerelease.tag_name;
    }
    return releases[0].tag_name;
  }

  if (channel === 'latest') {
    return releases[0].tag_name;
  }

  throw new Error(`Unsupported release channel: ${channel}`);
}

async function downloadToFile(url, destination, options = {}) {
  const { body } = await request(url, options);
  await fsp.mkdir(path.dirname(destination), { recursive: true });
  await fsp.writeFile(destination, body);
}

async function sha256File(filePath) {
  const hash = crypto.createHash('sha256');
  const file = await fsp.readFile(filePath);
  hash.update(file);
  return hash.digest('hex');
}

async function verifyChecksum(filePath, checksumUrl, options = {}) {
  try {
    const checksumText = await requestText(checksumUrl, options);
    const expected = checksumText.trim().split(/\s+/)[0];
    if (!expected) {
      return;
    }
    const actual = await sha256File(filePath);
    if (expected !== actual) {
      throw new Error(
        `Checksum mismatch for ${path.basename(filePath)}: expected ${expected}, got ${actual}`
      );
    }
  } catch (error) {
    if (error && error.statusCode === 404) {
      return;
    }
    throw error;
  }
}

async function downloadBinary(options = {}) {
  const repo = options.repo || DEFAULT_REPO;
  const assetName = options.assetName || resolveAssetName(options);
  const destination = resolveManagedBinaryPath(options);
  const releaseTag = await getReleaseTag(options);
  const githubToken = options.githubToken || process.env.GITHUB_TOKEN;
  const headers = githubToken ? { Authorization: `Bearer ${githubToken}` } : {};
  const assetUrl = `https://github.com/${repo}/releases/download/${releaseTag}/${assetName}`;
  const checksumUrl = `${assetUrl}.sha256`;

  await downloadToFile(assetUrl, destination, { headers });

  if (process.platform !== 'win32') {
    await fsp.chmod(destination, 0o755);
  }

  if (options.verify !== false) {
    await verifyChecksum(destination, checksumUrl, { headers });
  }

  return destination;
}

async function resolveBinaryWithInfo(options = {}) {
  if (options.binaryPath) {
    const binaryPath = path.resolve(options.binaryPath);
    if (!exists(binaryPath)) {
      throw new Error(`reddb binary not found at ${binaryPath}`);
    }
    return {
      binaryPath,
      source: 'explicit'
    };
  }

  const binaryName = options.binaryName || DEFAULT_BINARY_NAME;
  const installedCandidate = resolveManagedBinaryPath(options);
  if (exists(installedCandidate)) {
    return {
      binaryPath: installedCandidate,
      source: 'managed'
    };
  }

  const packageCandidate = resolvePackageLocalBinaryPath(options);
  if (exists(packageCandidate)) {
    return {
      binaryPath: packageCandidate,
      source: 'package'
    };
  }

  const legacyCandidate = resolveLegacyBinaryPath(options);
  if (legacyCandidate && exists(legacyCandidate)) {
    return {
      binaryPath: legacyCandidate,
      source: 'legacy'
    };
  }

  const pathCandidate = resolveFromPath(binaryName);
  if (pathCandidate) {
    return {
      binaryPath: pathCandidate,
      source: 'path'
    };
  }

  if (options.autoDownload) {
    return {
      binaryPath: await downloadBinary(options),
      source: 'downloaded'
    };
  }

  throw new Error(
    `Unable to resolve reddb binary. Set binaryPath, provide autoDownload=true, or install ${binaryName} in PATH.`
  );
}

async function resolveBinary(options = {}) {
  const resolved = await resolveBinaryWithInfo(options);
  return resolved.binaryPath;
}

function normalizeReleaseTag(value) {
  if (!value) {
    return null;
  }

  const stringValue = String(value).trim();
  if (!stringValue) {
    return null;
  }

  return stringValue.startsWith('v') ? stringValue : `v${stringValue}`;
}

function parseInstalledVersion(output) {
  const text = String(output || '').trim();
  if (!text) {
    return null;
  }

  const match = text.match(/\bv\d+\.\d+\.\d+(?:[-+][^\s]+)?\b/);
  if (match) {
    return match[0];
  }

  const bare = text.match(/\b\d+\.\d+\.\d+(?:[-+][^\s]+)?\b/);
  return bare ? `v${bare[0]}` : null;
}

async function getInstalledVersion(binaryPath, options = {}) {
  try {
    const result = await execFilePromise(binaryPath, ['--version'], options);
    return parseInstalledVersion(result.stdout || result.stderr);
  } catch (_) {
    return null;
  }
}

async function getBinaryInfo(options = {}) {
  const resolved = await resolveBinaryWithInfo(options);
  const version = await getInstalledVersion(resolved.binaryPath, options);
  return {
    binaryPath: resolved.binaryPath,
    source: resolved.source,
    version
  };
}

function resolveManagedUpgradeDestination(options = {}, currentInfo = null) {
  if (options.binaryPath) {
    return path.resolve(options.binaryPath);
  }

  if (options.targetDir) {
    return resolveManagedBinaryPath(options);
  }

  if (currentInfo && (currentInfo.source === 'managed' || currentInfo.source === 'legacy')) {
    return currentInfo.binaryPath;
  }

  return resolveManagedBinaryPath(options);
}

async function ensureInstalled(options = {}) {
  try {
    const info = await getBinaryInfo(Object.assign({}, options, { autoDownload: false }));
    return Object.assign({ changed: false }, info);
  } catch (_) {
    const releaseTag = normalizeReleaseTag(options.releaseVersion || options.version) || (await getReleaseTag(options));
    const binaryPath = await downloadBinary(
      Object.assign({}, options, {
        version: releaseTag
      })
    );
    return {
      binaryPath,
      source: 'downloaded',
      version: releaseTag,
      changed: true
    };
  }
}

async function checkForUpdates(options = {}) {
  const releaseTag = await getReleaseTag(options);
  let current = null;

  try {
    current = await getBinaryInfo(Object.assign({}, options, { autoDownload: false }));
  } catch (_) {
    current = null;
  }

  return {
    binaryPath: current ? current.binaryPath : resolveManagedBinaryPath(options),
    currentVersion: current ? current.version : null,
    latestVersion: releaseTag,
    source: current ? current.source : null,
    hasUpdate: !current || current.version !== releaseTag
  };
}

async function upgradeBinary(options = {}) {
  const releaseTag = await getReleaseTag(options);
  let current = null;

  try {
    current = await getBinaryInfo(Object.assign({}, options, { autoDownload: false }));
  } catch (_) {
    current = null;
  }

  const destination = resolveManagedUpgradeDestination(options, current);
  const currentVersion = current ? current.version : null;
  const needsDownload = options.force === true || !exists(destination) || currentVersion !== releaseTag;

  if (!needsDownload) {
    return {
      binaryPath: destination,
      previousVersion: currentVersion,
      version: releaseTag,
      changed: false,
      source: current.source
    };
  }

  const binaryPath = await downloadBinary(
    Object.assign({}, options, {
      binaryPath: destination
    })
  );

  return {
    binaryPath,
    previousVersion: currentVersion,
    version: releaseTag,
    changed: true,
    source: current ? current.source : 'managed'
  };
}

function execFilePromise(binaryPath, args, options = {}) {
  return new Promise((resolve, reject) => {
    execFile(
      binaryPath,
      args,
      {
        cwd: options.cwd,
        env: options.env,
        timeout: options.timeout,
        maxBuffer: options.maxBuffer || 32 * 1024 * 1024
      },
      (error, stdout, stderr) => {
        if (error) {
          error.stdout = stdout;
          error.stderr = stderr;
          reject(error);
          return;
        }

        resolve({
          code: 0,
          stdout,
          stderr,
          args: [binaryPath].concat(args)
        });
      }
    );
  });
}

function spawnBinary(binaryPath, args, options = {}) {
  return spawn(binaryPath, args, {
    cwd: options.cwd,
    env: options.env,
    stdio: options.stdio || 'inherit',
    detached: options.detached === true
  });
}

function waitForChild(child) {
  return new Promise((resolve, reject) => {
    child.on('error', reject);
    child.on('close', (code, signal) => {
      if (signal) {
        resolve(1);
        return;
      }
      resolve(code);
    });
  });
}

function buildFlags(flags = {}) {
  const args = [];
  for (const [key, value] of Object.entries(flags)) {
    if (value === undefined || value === null || value === false) {
      continue;
    }
    const flag = `--${key}`;
    if (value === true) {
      args.push(flag);
    } else if (Array.isArray(value)) {
      for (const item of value) {
        args.push(flag, String(item));
      }
    } else {
      args.push(flag, String(value));
    }
  }
  return args;
}

async function runJson(binaryPath, args, options = {}) {
  const result = await execFilePromise(binaryPath, args, options);
  const stdout = String(result.stdout || '').trim();

  if (!stdout) {
    return null;
  }

  try {
    return JSON.parse(stdout);
  } catch (error) {
    const wrapped = new Error(`reddb command did not emit valid JSON: ${error.message}`);
    wrapped.stdout = stdout;
    wrapped.stderr = result.stderr;
    wrapped.args = args;
    throw wrapped;
  }
}

async function createClient(options = {}) {
  const defaults = ensureObject(options, 'createClient options');
  const binaryPath = await resolveBinary(defaults);

  return {
    $binaryPath: binaryPath,

    // Server management
    async server(flags = {}) {
      return runJson(binaryPath, ['server', ...buildFlags(flags)], defaults);
    },

    // Query
    async query(sql, flags = {}) {
      return runJson(binaryPath, ['query', sql, '--json', ...buildFlags(flags)], defaults);
    },

    // Entity operations
    async insert(collection, data, flags = {}) {
      return runJson(binaryPath, ['insert', collection, JSON.stringify(data), '--json', ...buildFlags(flags)], defaults);
    },

    async get(collection, id, flags = {}) {
      return runJson(binaryPath, ['get', collection, id, '--json', ...buildFlags(flags)], defaults);
    },

    async delete(collection, id, flags = {}) {
      return runJson(binaryPath, ['delete', collection, id, '--json', ...buildFlags(flags)], defaults);
    },

    // Health
    async health(flags = {}) {
      return runJson(binaryPath, ['health', '--json', ...buildFlags(flags)], defaults);
    },

    // Auth
    async auth(subcommand, flags = {}) {
      return runJson(binaryPath, ['auth', subcommand, '--json', ...buildFlags(flags)], defaults);
    },

    // Connect (returns spawn for interactive REPL)
    connect(addr, flags = {}) {
      return spawnBinary(binaryPath, ['connect', addr, ...buildFlags(flags)], { stdio: 'inherit' });
    },

    // MCP
    mcp(flags = {}) {
      return spawnBinary(binaryPath, ['mcp', ...buildFlags(flags)], { stdio: ['pipe', 'pipe', 'inherit'] });
    },

    // Version
    async version() {
      return runJson(binaryPath, ['version', '--json'], defaults);
    },

    // Raw exec
    async exec(args) {
      return execFilePromise(binaryPath, args, defaults);
    },

    // Raw spawn
    spawn(args, opts) {
      return spawnBinary(binaryPath, args, opts);
    }
  };
}

async function runCli(argv = process.argv.slice(2), runtime = {}) {
  const stderr = runtime.stderr || process.stderr;

  try {
    const rawArgs = Array.isArray(argv) ? argv.slice() : [];
    const cliOptions = {
      cwd: runtime.cwd || process.cwd(),
      env: Object.assign({}, process.env, runtime.env || {})
    };
    const binaryPath = await resolveBinary(cliOptions);
    const spawnOptions = {
      cwd: runtime.cwd || process.cwd(),
      env: Object.assign({}, process.env, runtime.env || {}),
      stdio: runtime.stdio || 'inherit'
    };
    const child = spawnBinary(binaryPath, rawArgs, spawnOptions);

    return waitForChild(child);
  } catch (error) {
    stderr.write(`reddb: ${error.message}\n`);
    return 1;
  }
}

module.exports = {
  version: SDK_VERSION,
  checkForUpdates,
  createClient,
  downloadBinary,
  ensureInstalled,
  getBinaryInfo,
  getInstalledVersion,
  runCli,
  runJson,
  resolveAssetName,
  resolveBinary,
  resolveBinaryWithInfo,
  upgradeBinary,
  execFile: execFilePromise,
  spawnChild: spawnBinary
};

module.exports._internal = {
  buildFlags,
  checkForUpdates,
  defaultInstallDir,
  downloadToFile,
  ensureInstalled,
  ensureObject,
  execFilePromise,
  exists,
  getBinaryInfo,
  getDefaultBinaryName,
  getInstalledVersion,
  getReleaseTag,
  isExecutable,
  kebabToCamel,
  legacyInstallDir,
  normalizeReleaseTag,
  parseInstalledVersion,
  request,
  requestJson,
  requestText,
  resolveFromPath,
  resolveBinaryWithInfo,
  resolveLegacyBinaryPath,
  resolvePackageLocalBinaryPath,
  resolveManagedBinaryPath,
  resolveManagedUpgradeDestination,
  sha256File,
  spawnBinary,
  upgradeBinary,
  waitForChild,
  verifyChecksum
};

module.exports.default = module.exports;

if (require.main === module) {
  runCli().then((code) => {
    process.exitCode = code;
  });
}

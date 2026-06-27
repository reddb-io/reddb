/**
 * Spawn `red mcp` over stdio.
 *
 * The launcher reimplements no tool/knowledge logic — it simply execs the
 * native engine's `mcp` subcommand and inherits stdio so the MCP JSON-RPC
 * stream flows straight through to the parent (the agent host). Any extra
 * argv passed to `npx @reddb-io/mcp ...` is forwarded after `mcp` (e.g.
 * `--uri <addr>` for connection mode selection).
 *
 * `spawn` is injectable so tests can assert the binary, argv, and stdio
 * wiring without launching a real process.
 */

import { spawn as nodeSpawn } from 'node:child_process'

/**
 * @param {string} binary absolute path to the `red` binary
 * @param {string[]} [extraArgs] argv forwarded after the `mcp` subcommand
 * @param {{
 *   spawn?: typeof nodeSpawn,
 *   stdio?: import('node:child_process').StdioOptions,
 *   env?: NodeJS.ProcessEnv,
 *   cwd?: string,
 * }} [opts]
 * @returns {import('node:child_process').ChildProcess}
 */
export function spawnMcp(
  binary,
  extraArgs = [],
  { spawn = nodeSpawn, stdio = 'inherit', env = process.env, cwd = process.cwd() } = {},
) {
  if (typeof binary !== 'string' || binary === '') {
    throw new TypeError('spawnMcp: `binary` must be a non-empty string')
  }
  return spawn(binary, redMcpArgs(extraArgs, env), { stdio, env, cwd })
}

export function redMcpArgs(extraArgs = [], env = process.env) {
  if (hasFlag(extraArgs, 'uri')) {
    return ['mcp', ...extraArgs]
  }

  const uri = env.REDDB_MCP_URI
  if (typeof uri === 'string' && uri !== '') {
    return ['mcp', '--uri', uri, ...extraArgs]
  }

  return ['mcp', ...extraArgs]
}

function hasFlag(args, name) {
  const long = `--${name}`
  return args.some((arg) => arg === long || arg.startsWith(`${long}=`))
}

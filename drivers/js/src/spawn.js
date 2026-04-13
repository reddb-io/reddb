/**
 * Runtime-agnostic subprocess spawn.
 *
 * Returns a uniform handle exposing:
 *   - `stdin`  WritableStream-like (has `write(buf)`, `end()`)
 *   - `stdout` AsyncIterable<Uint8Array> for line-buffered reading
 *   - `kill()` terminate the process
 *   - `wait()` resolve when the process exits
 *
 * Detects Bun and Deno first because they ship native, Node-incompatible
 * spawn APIs that are faster than their `node:child_process` shims.
 */

const isBun = typeof globalThis.Bun !== 'undefined' && typeof globalThis.Bun.spawn === 'function'
const isDeno = typeof globalThis.Deno !== 'undefined' && typeof globalThis.Deno.Command === 'function'

/** @returns {Promise<RedProcess>} */
export async function spawnRed(binary, args) {
  if (isBun) return spawnBun(binary, args)
  if (isDeno) return spawnDeno(binary, args)
  return spawnNode(binary, args)
}

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

async function spawnNode(binary, args) {
  const { spawn } = await import('node:child_process')
  const child = spawn(binary, args, { stdio: ['pipe', 'pipe', 'inherit'] })

  return {
    runtime: 'node',
    stdin: {
      write(buf) {
        return new Promise((resolve, reject) => {
          child.stdin.write(buf, (err) => (err ? reject(err) : resolve()))
        })
      },
      end() {
        child.stdin.end()
      },
    },
    stdout: child.stdout, // already AsyncIterable<Buffer>
    kill() {
      child.kill('SIGTERM')
    },
    wait() {
      return new Promise((resolve) => {
        child.on('exit', (code) => resolve(code ?? 0))
      })
    },
  }
}

// ---------------------------------------------------------------------------
// Bun
// ---------------------------------------------------------------------------

function spawnBun(binary, args) {
  const child = globalThis.Bun.spawn({
    cmd: [binary, ...args],
    stdin: 'pipe',
    stdout: 'pipe',
    stderr: 'inherit',
  })

  const writer = child.stdin.getWriter ? child.stdin.getWriter() : null

  return {
    runtime: 'bun',
    stdin: {
      async write(buf) {
        if (writer) {
          await writer.write(buf)
        } else {
          // Older Bun: stdin is a FileSink
          child.stdin.write(buf)
          await child.stdin.flush()
        }
      },
      end() {
        if (writer) {
          writer.close()
        } else {
          child.stdin.end()
        }
      },
    },
    stdout: bunStdoutToAsyncIterable(child.stdout),
    kill() {
      child.kill()
    },
    wait() {
      return child.exited
    },
  }
}

async function* bunStdoutToAsyncIterable(stream) {
  const reader = stream.getReader()
  try {
    while (true) {
      const { value, done } = await reader.read()
      if (done) return
      yield value
    }
  } finally {
    reader.releaseLock()
  }
}

// ---------------------------------------------------------------------------
// Deno
// ---------------------------------------------------------------------------

async function spawnDeno(binary, args) {
  const cmd = new globalThis.Deno.Command(binary, {
    args,
    stdin: 'piped',
    stdout: 'piped',
    stderr: 'inherit',
  })
  const child = cmd.spawn()
  const writer = child.stdin.getWriter()

  return {
    runtime: 'deno',
    stdin: {
      async write(buf) {
        await writer.write(buf)
      },
      end() {
        try {
          writer.close()
        } catch {
          // already closed
        }
      },
    },
    stdout: denoStdoutToAsyncIterable(child.stdout),
    kill() {
      try {
        child.kill('SIGTERM')
      } catch {
        // process may already be gone
      }
    },
    async wait() {
      const status = await child.status
      return status.code ?? 0
    },
  }
}

async function* denoStdoutToAsyncIterable(stream) {
  const reader = stream.getReader()
  try {
    while (true) {
      const { value, done } = await reader.read()
      if (done) return
      yield value
    }
  } finally {
    reader.releaseLock()
  }
}

/**
 * @typedef {{
 *   runtime: 'node' | 'bun' | 'deno',
 *   stdin: { write(buf: Uint8Array): Promise<void>, end(): void },
 *   stdout: AsyncIterable<Uint8Array>,
 *   kill(): void,
 *   wait(): Promise<number>,
 * }} RedProcess
 */

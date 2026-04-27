package dev.reddb

import kotlinx.coroutines.runBlocking
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assertions.fail
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.condition.EnabledIfEnvironmentVariable
import java.io.BufferedReader
import java.io.File
import java.io.InputStream
import java.io.InputStreamReader
import java.nio.charset.StandardCharsets
import java.util.concurrent.TimeUnit
import java.util.regex.Pattern

/**
 * End-to-end smoke against a freshly-spawned RedDB binary. Gated on
 * `RED_SMOKE=1` so normal test runs don't drag in cargo build time.
 */
@EnabledIfEnvironmentVariable(named = "RED_SMOKE", matches = "1")
class SmokeTest {

    private val portRe: Pattern = Pattern.compile("(?:tcp://|listening on .*?:|port=)(\\d{2,5})")

    @Test
    fun runsAgainstRealEngine() {
        val repoRoot = findRepoRoot()
        val pb = ProcessBuilder(
            "cargo", "run", "--release", "--bin", "red", "--",
            "serve", "--bind", "127.0.0.1:0", "--anon-ok"
        ).directory(repoRoot).redirectErrorStream(true)
        val proc = pb.start()
        try {
            val port = waitForPort(proc.inputStream, 60_000)
            runBlocking {
                connect("red://127.0.0.1:$port").use { conn ->
                    conn.ping()
                    conn.insert("smoke_users", mapOf("name" to "alice", "age" to 30))
                    val result = conn.query("SELECT * FROM smoke_users WHERE name = 'alice'")
                    val body = String(result, StandardCharsets.UTF_8)
                    assertTrue(body.contains("alice"), "expected alice in: $body")
                    conn.delete("smoke_users", "alice")
                }
            }
        } finally {
            proc.destroy()
            if (!proc.waitFor(10, TimeUnit.SECONDS)) {
                proc.destroyForcibly()
            }
        }
    }

    private fun waitForPort(input: InputStream, timeoutMs: Long): Int {
        val deadline = System.currentTimeMillis() + timeoutMs
        val br = BufferedReader(InputStreamReader(input, StandardCharsets.UTF_8))
        while (System.currentTimeMillis() < deadline) {
            val line = br.readLine() ?: break
            val m = portRe.matcher(line)
            if (m.find()) return m.group(1).toInt()
        }
        throw java.io.IOException("never saw a bind port in engine stdout")
    }

    private fun findRepoRoot(): File {
        var f: File? = File("").absoluteFile
        while (f != null) {
            if (File(f, "Cargo.toml").exists() && File(f, "drivers").isDirectory) return f
            f = f.parentFile
        }
        fail<Nothing>("could not locate repo root with Cargo.toml + drivers/")
        error("unreachable")
    }
}

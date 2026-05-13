package dev.reddb

import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assertions.fail
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.condition.EnabledIfEnvironmentVariable
import java.io.File
import java.net.InetAddress
import java.net.ServerSocket
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import java.time.Duration
import java.util.concurrent.TimeUnit

/**
 * End-to-end smoke against a freshly-spawned RedDB binary. Gated on
 * `RED_SMOKE=1` so normal test runs don't drag in cargo build time.
 */
@EnabledIfEnvironmentVariable(named = "RED_SMOKE", matches = "1")
class SmokeTest {

    @Test
    fun runsAgainstRealEngine() {
        val repoRoot = findRepoRoot()
        val dataDir = Files.createTempDirectory("reddb-kotlin-smoke-")
        val port = freePort()
        val pb = ProcessBuilder(redCommand(dataDir.resolve("data.db"), port))
            .directory(repoRoot)
            .redirectErrorStream(true)
            .redirectOutput(ProcessBuilder.Redirect.DISCARD)
        val proc = pb.start()
        try {
            runBlocking {
                waitForConnect("red://127.0.0.1:$port", Duration.ofSeconds(60)).use { conn ->
                    conn.ping()
                    conn.query("CREATE TABLE smoke_params (id INT, name TEXT)")
                    conn.query("INSERT INTO smoke_params (id, name) VALUES (\$1, \$2)", 42, "alice")
                    val result = conn.query("SELECT 1")
                    val body = String(result, StandardCharsets.UTF_8)
                    assertTrue(body.contains(""""ok":true"""), "expected ok result in: $body")
                    val paramResult = conn.query(
                        "SELECT name FROM smoke_params WHERE id = \$1 AND name = \$2",
                        42,
                        "alice",
                    )
                    val paramBody = String(paramResult, StandardCharsets.UTF_8)
                    assertTrue(paramBody.contains("alice"), "expected parameterized alice in: $paramBody")
                    val preparedResult = conn.prepare("SELECT name FROM smoke_params WHERE id = \$1 AND name = \$2")
                        .bind(42)
                        .bind("alice")
                        .query()
                    val preparedBody = String(preparedResult, StandardCharsets.UTF_8)
                    assertTrue(preparedBody.contains("alice"), "expected prepared alice in: $preparedBody")
                    val embedding = floatArrayOf(0.7f, 0.7f)
                    conn.query(
                        "INSERT INTO smoke_embeddings VECTOR (dense, content) VALUES (\$1, \$2)",
                        embedding,
                        "parameterized doc",
                    )
                    val vectorResult = conn.query(
                        "SEARCH SIMILAR \$1 COLLECTION smoke_embeddings LIMIT 1",
                        listOf(0.7f, 0.7f),
                    )
                    val vectorBody = String(vectorResult, StandardCharsets.UTF_8)
                    assertTrue(vectorBody.contains(""""record_count":1"""), "expected vector hit in: $vectorBody")
                }
            }
        } finally {
            proc.destroy()
            if (!proc.waitFor(10, TimeUnit.SECONDS)) {
                proc.destroyForcibly()
            }
        }
    }

    private fun redCommand(dataPath: Path, port: Int): List<String> {
        val redBin = System.getenv("RED_BIN")
        val cmd = mutableListOf<String>()
        if (!redBin.isNullOrBlank()) {
            cmd += redBin
        } else {
            cmd += listOf("cargo", "run", "--release", "--bin", "red", "--")
        }
        cmd += listOf("server", "--path", dataPath.toString(), "--bind", "127.0.0.1:$port")
        return cmd
    }

    private suspend fun waitForConnect(uri: String, timeout: Duration): Conn {
        val deadline = System.nanoTime() + timeout.toNanos()
        var last: Exception? = null
        while (System.nanoTime() < deadline) {
            try {
                val conn = connect(uri)
                try {
                    conn.ping()
                    return conn
                } catch (e: Exception) {
                    conn.close()
                    last = e
                }
            } catch (e: Exception) {
                last = e
            }
            delay(50)
        }
        throw java.io.IOException("server did not accept connections at $uri", last)
    }

    private fun freePort(): Int {
        ServerSocket(0, 0, InetAddress.getByName("127.0.0.1")).use { socket ->
            return socket.localPort
        }
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

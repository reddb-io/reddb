package dev.reddb

import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path

class EmbeddedTest {
    @TempDir lateinit var directory: Path

    @Test
    fun memoryAndSingleFileJourney(): Unit = runTest {
        assumeTrue(System.getenv("REDDB_EMBEDDED_SMOKE") == "1")

        connect("memory://").use { memory ->
            memory.query("CREATE TABLE kotlin_embedded (id INTEGER, name TEXT)")
            memory.query("INSERT INTO kotlin_embedded (id, name) VALUES ($1, $2)", 7, "embedded")
            val rows = memory.query(
                "SELECT name FROM kotlin_embedded WHERE id = $1", 7
            ).toString(Charsets.UTF_8)
            assertTrue(rows.contains("embedded"), rows)
        }

        val database = directory.resolve("driver-contract.rdb")
        val uri = database.toUri().toString()
        val first = connect(uri)
        first.query("CREATE TABLE durable (id INTEGER, name TEXT)")
        first.query("INSERT INTO durable (id, name) VALUES (1, 'persisted')")
        var rejected = false
        try {
            connect(uri).close()
        } catch (_: RedDBException.EngineError) {
            rejected = true
        }
        assertTrue(rejected, "second writer unexpectedly opened")
        first.close()

        connect(uri).use { reopened ->
            val rows = reopened.query(
                "SELECT name FROM durable WHERE id = 1"
            ).toString(Charsets.UTF_8)
            assertTrue(rows.contains("persisted"), rows)
        }
        Files.list(directory).use { entries -> assertEquals(1, entries.count()) }
    }
}

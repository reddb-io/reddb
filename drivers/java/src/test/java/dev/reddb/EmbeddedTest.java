package dev.reddb;

import org.junit.jupiter.api.Assumptions;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;

import static org.junit.jupiter.api.Assertions.*;

class EmbeddedTest {
    @TempDir Path directory;

    @Test
    void memoryAndSingleFileJourney() throws Exception {
        Assumptions.assumeTrue("1".equals(System.getenv("REDDB_EMBEDDED_SMOKE")));

        try (Conn memory = Reddb.connect("memory://")) {
            memory.query("CREATE TABLE java_embedded (id INTEGER, name TEXT)");
            memory.query("INSERT INTO java_embedded (id, name) VALUES ($1, $2)", 7, "embedded");
            String rows = new String(memory.query(
                "SELECT name FROM java_embedded WHERE id = $1", 7), StandardCharsets.UTF_8);
            assertTrue(rows.contains("embedded"), rows);
        }

        Path database = directory.resolve("driver-contract.rdb");
        String uri = database.toUri().toString();
        Conn first = Reddb.connect(uri);
        first.query("CREATE TABLE durable (id INTEGER, name TEXT)");
        first.query("INSERT INTO durable (id, name) VALUES (1, 'persisted')");
        assertThrows(RedDBException.EngineError.class, () -> Reddb.connect(uri));
        first.close();

        try (Conn reopened = Reddb.connect(uri)) {
            String rows = new String(reopened.query(
                "SELECT name FROM durable WHERE id = 1"), StandardCharsets.UTF_8);
            assertTrue(rows.contains("persisted"), rows);
        }
        try (var entries = Files.list(directory)) {
            assertEquals(1, entries.count());
        }
    }
}

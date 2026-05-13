package dev.reddb;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.condition.EnabledIfEnvironmentVariable;

import java.io.File;
import java.io.IOException;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;

import static org.junit.jupiter.api.Assertions.*;

/**
 * End-to-end smoke against a freshly-spawned RedDB binary. Gated on
 * {@code RED_SMOKE=1} so normal test runs don't drag in cargo build
 * time.
 */
@EnabledIfEnvironmentVariable(named = "RED_SMOKE", matches = "1")
class SmokeTest {

    @Test
    void runsAgainstRealEngine() throws Exception {
        File repoRoot = findRepoRoot();
        Path dataDir = Files.createTempDirectory("reddb-java-smoke-");
        int port = freePort();
        ProcessBuilder pb = new ProcessBuilder(redCommand(dataDir.resolve("data.db"), port));
        pb.directory(repoRoot);
        pb.redirectErrorStream(true);
        pb.redirectOutput(ProcessBuilder.Redirect.DISCARD);
        Process proc = pb.start();
        try {
            try (Conn conn = waitForConnect("red://127.0.0.1:" + port, Duration.ofSeconds(60))) {
                conn.ping();
                conn.query("CREATE TABLE smoke_params (id INT, name TEXT)");
                conn.query("INSERT INTO smoke_params (id, name) VALUES ($1, $2)", 42, "alice");
                byte[] result = conn.query("SELECT 1");
                String body = new String(result, StandardCharsets.UTF_8);
                assertTrue(body.contains("\"ok\":true"), "expected ok result in: " + body);
                byte[] paramResult = conn.query(
                    "SELECT name FROM smoke_params WHERE id = $1 AND name = $2",
                    42,
                    "alice"
                );
                String paramBody = new String(paramResult, StandardCharsets.UTF_8);
                assertTrue(paramBody.contains("alice"), "expected parameterized alice in: " + paramBody);
                byte[] preparedResult = conn.prepare("SELECT name FROM smoke_params WHERE id = $1 AND name = $2")
                    .bind(42)
                    .bind("alice")
                    .query();
                String preparedBody = new String(preparedResult, StandardCharsets.UTF_8);
                assertTrue(preparedBody.contains("alice"), "expected prepared alice in: " + preparedBody);
            }
        } finally {
            proc.destroy();
            if (!proc.waitFor(10, java.util.concurrent.TimeUnit.SECONDS)) {
                proc.destroyForcibly();
            }
        }
    }

    private static List<String> redCommand(Path dataPath, int port) {
        String redBin = System.getenv("RED_BIN");
        List<String> cmd = new ArrayList<>();
        if (redBin != null && !redBin.isBlank()) {
            cmd.add(redBin);
        } else {
            cmd.add("cargo");
            cmd.add("run");
            cmd.add("--release");
            cmd.add("--bin");
            cmd.add("red");
            cmd.add("--");
        }
        cmd.add("server");
        cmd.add("--path");
        cmd.add(dataPath.toString());
        cmd.add("--bind");
        cmd.add("127.0.0.1:" + port);
        return cmd;
    }

    private static Conn waitForConnect(String uri, Duration timeout) throws Exception {
        long deadline = System.nanoTime() + timeout.toNanos();
        RuntimeException last = null;
        while (System.nanoTime() < deadline) {
            try {
                Conn conn = Reddb.connect(uri);
                try {
                    conn.ping();
                    return conn;
                } catch (RuntimeException e) {
                    conn.close();
                    last = e;
                }
            } catch (RuntimeException e) {
                last = e;
            }
            Thread.sleep(50);
        }
        IOException error = new IOException("server did not accept connections at " + uri);
        if (last != null) error.initCause(last);
        throw error;
    }

    private static int freePort() throws IOException {
        try (ServerSocket socket = new ServerSocket(0, 0, InetAddress.getByName("127.0.0.1"))) {
            return socket.getLocalPort();
        }
    }

    private static File findRepoRoot() {
        File f = new File("").getAbsoluteFile();
        while (f != null) {
            if (new File(f, "Cargo.toml").exists() && new File(f, "drivers").isDirectory()) {
                return f;
            }
            f = f.getParentFile();
        }
        fail("could not locate repo root with Cargo.toml + drivers/");
        return null; // unreachable
    }
}

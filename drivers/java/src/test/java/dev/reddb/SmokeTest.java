package dev.reddb;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.condition.EnabledIfEnvironmentVariable;

import java.io.BufferedReader;
import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

import static org.junit.jupiter.api.Assertions.*;

/**
 * End-to-end smoke against a freshly-spawned RedDB binary. Gated on
 * {@code RED_SMOKE=1} so normal test runs don't drag in cargo build
 * time. The test discovers the bind port from stdout — the engine
 * prints `listening on tcp://127.0.0.1:<port>` once the listener is
 * up.
 */
@EnabledIfEnvironmentVariable(named = "RED_SMOKE", matches = "1")
class SmokeTest {

    private static final Pattern PORT_RE = Pattern.compile("(?:tcp://|listening on .*?:|port=)(\\d{2,5})");

    @Test
    void runsAgainstRealEngine() throws Exception {
        File repoRoot = findRepoRoot();
        ProcessBuilder pb = new ProcessBuilder(
            "cargo", "run", "--release", "--bin", "red", "--",
            "serve", "--bind", "127.0.0.1:0", "--anon-ok"
        );
        pb.directory(repoRoot);
        pb.redirectErrorStream(true);
        Process proc = pb.start();
        try {
            int port = waitForPort(proc.getInputStream(), 60_000);
            try (Conn conn = Reddb.connect("red://127.0.0.1:" + port)) {
                conn.ping();
                conn.insert("smoke_users", Map.of("name", "alice", "age", 30));
                byte[] result = conn.query("SELECT * FROM smoke_users WHERE name = 'alice'");
                String body = new String(result, StandardCharsets.UTF_8);
                assertTrue(body.contains("alice"), "expected alice in: " + body);
                conn.delete("smoke_users", "alice");
            }
        } finally {
            proc.destroy();
            if (!proc.waitFor(10, java.util.concurrent.TimeUnit.SECONDS)) {
                proc.destroyForcibly();
            }
        }
    }

    private static int waitForPort(InputStream in, long timeoutMs) throws IOException {
        long deadline = System.currentTimeMillis() + timeoutMs;
        BufferedReader br = new BufferedReader(new InputStreamReader(in, StandardCharsets.UTF_8));
        String line;
        while (System.currentTimeMillis() < deadline && (line = br.readLine()) != null) {
            Matcher m = PORT_RE.matcher(line);
            if (m.find()) {
                return Integer.parseInt(m.group(1));
            }
        }
        throw new IOException("never saw a bind port in engine stdout");
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

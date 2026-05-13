import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.Statement;

public final class JdbcClient {
    public static void main(String[] args) throws Exception {
        String port = System.getenv("PGPORT");
        String url = "jdbc:postgresql://127.0.0.1:" + port + "/reddb"
                + "?sslmode=disable&preferQueryMode=extended&prepareThreshold=1"
                + "&ApplicationName=pgwire360-jdbc";
        try (Connection conn = DriverManager.getConnection(url, "reddb", "")) {
            conn.setAutoCommit(true);
            try (Statement st = conn.createStatement()) {
                st.execute("CREATE TABLE jdbc_items (id INT, name TEXT)");
            }
            try (PreparedStatement ps = conn.prepareStatement(
                    "INSERT INTO jdbc_items (id, name) VALUES (?::int, ?::text)")) {
                ps.setInt(1, 1);
                ps.setString(2, "alice");
                if (ps.executeUpdate() != 1) {
                    throw new AssertionError("insert affected row mismatch");
                }
            }
            try (PreparedStatement ps = conn.prepareStatement(
                    "SELECT name FROM jdbc_items WHERE id = ?::int")) {
                ps.setInt(1, 1);
                try (ResultSet rs = ps.executeQuery()) {
                    if (!rs.next()
                            || !"alice".equals(rs.getString(1))) {
                        throw new AssertionError("select row mismatch");
                    }
                }
            }
            try (Statement st = conn.createStatement()) {
                st.execute("INSERT INTO jdbc_vec VECTOR (dense, content) VALUES ([1.0, 0.0], 'gateway')");
                st.execute("INSERT INTO jdbc_vec VECTOR (dense, content) VALUES ([0.0, 1.0], 'database')");
            }
            try (PreparedStatement ps = conn.prepareStatement(
                    "SEARCH SIMILAR [1.0, 0.0] COLLECTION jdbc_vec LIMIT ?::int")) {
                ps.setInt(1, 1);
                try (ResultSet rs = ps.executeQuery()) {
                    if (!rs.next()) {
                        throw new AssertionError("vector row missing");
                    }
                }
            }
            try (PreparedStatement ps = conn.prepareStatement(
                    "ASK ?::text STRICT OFF LIMIT 1")) {
                ps.setString(1, "why did incident FDD-12313 fail?");
                try (ResultSet rs = ps.executeQuery()) {
                    ResultSetMetaData md = rs.getMetaData();
                    String[] expected = {
                            "answer",
                            "cache_hit",
                            "citations",
                            "completion_tokens",
                            "cost_usd",
                            "mode",
                            "model",
                            "prompt_tokens",
                            "provider",
                            "retry_count",
                            "sources_flat",
                            "validation"
                    };
                    if (md.getColumnCount() != expected.length) {
                        throw new AssertionError("ASK column count mismatch");
                    }
                    for (int i = 0; i < expected.length; i++) {
                        if (!expected[i].equals(md.getColumnName(i + 1))) {
                            throw new AssertionError("ASK column mismatch at " + (i + 1));
                        }
                    }
                    if (!rs.next()
                            || !"mock response".equals(rs.getString("answer"))
                            || !"openai".equals(rs.getString("provider"))
                            || rs.getString("sources_flat") == null
                            || rs.getString("validation") == null) {
                        throw new AssertionError("ASK row mismatch");
                    }
                }
            }
        }
    }
}

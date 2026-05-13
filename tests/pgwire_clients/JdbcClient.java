import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
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
        }
    }
}

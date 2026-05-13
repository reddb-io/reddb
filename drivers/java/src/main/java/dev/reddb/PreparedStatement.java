package dev.reddb;

import java.util.ArrayList;
import java.util.List;

public final class PreparedStatement {
    private final Conn conn;
    private final String sql;
    private final List<Object> params = new ArrayList<>();

    PreparedStatement(Conn conn, String sql) {
        if (conn == null) throw new IllegalArgumentException("conn is null");
        if (sql == null) throw new IllegalArgumentException("sql is null");
        this.conn = conn;
        this.sql = sql;
    }

    public PreparedStatement bind(Object value) {
        params.add(value);
        return this;
    }

    public PreparedStatement bind(int index, Object value) {
        if (index < 1) throw new IllegalArgumentException("index is 1-based");
        while (params.size() < index) {
            params.add(null);
        }
        params.set(index - 1, value);
        return this;
    }

    public PreparedStatement clear() {
        params.clear();
        return this;
    }

    public byte[] query() {
        return conn.query(sql, params.toArray());
    }
}

package dev.reddb;

import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Set;

final class RoutingSql {
    record Policy(String state, boolean read) {}
    static Policy policy(String sql) {
        List<String> tokens = tokens(sql);
        if (tokens == null || tokens.isEmpty() || tokens.contains(";")) return new Policy("session", false);
        String first = tokens.get(0);
        if (first.equals("BEGIN") || first.equals("START") && tokens.size() > 1 && tokens.get(1).equals("TRANSACTION")) return new Policy("begin", false);
        if (Set.of("COMMIT", "END", "ROLLBACK").contains(first)) {
            if (first.equals("ROLLBACK") && tokens.contains("TO")) return new Policy("primary", false);
            return new Policy(tokens.contains("CHAIN") && !tokens.contains("NO") ? "begin" : "end", false);
        }
        if (Set.of("SET", "RESET", "USE", "PREPARE", "DEALLOCATE", "DECLARE", "LISTEN").contains(first)) return new Policy("session", false);
        Set<String> forbidden = Set.of("INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "INTO", "FOR", "LOCK", "CALL", "EXECUTE", "SET", "QUEUE", "COPY", "[", "]");
        Set<String> functions = Set.of("COUNT", "SUM", "MIN", "MAX", "AVG", "LOWER", "UPPER", "LENGTH", "COALESCE", "ABS", "ROUND", "CAST", "EXISTS", "IN", "AS", "FROM", "WHERE", "AND", "OR", "NOT", "SELECT");
        boolean read = first.equals("SELECT") && tokens.stream().noneMatch(forbidden::contains);
        for (int index = 0; index + 1 < tokens.size() && read; index++) {
            String token = tokens.get(index);
            if (tokens.get(index + 1).equals("(") && (token.equals("#identifier") || Character.isLetter(token.charAt(0)) || token.charAt(0) == '_') && !functions.contains(token)) read = false;
        }
        return new Policy("primary", read);
    }
    private static List<String> tokens(String sql) {
        if (sql.length() > 1_048_576) return null;
        List<String> tokens = new ArrayList<>();
        int index = 0;
        while (index < sql.length()) {
            char current = sql.charAt(index);
            if (Character.isWhitespace(current)) { index++; continue; }
            if (sql.startsWith("--", index)) {
                int end = sql.indexOf('\n', index + 2); index = end < 0 ? sql.length() : end + 1; continue;
            }
            if (sql.startsWith("/*", index)) {
                int depth = 1; index += 2;
                while (index < sql.length() && depth > 0) {
                    if (sql.startsWith("/*", index)) { depth++; index += 2; }
                    else if (sql.startsWith("*/", index)) { depth--; index += 2; }
                    else index++;
                    if (depth > 64) return null;
                }
                if (depth != 0) return null;
                continue;
            }
            if (current == '\'' || current == '"' || current == '`') {
                index++; boolean closed = false;
                while (index < sql.length()) {
                    char value = sql.charAt(index++);
                    if (value == '\\') return null;
                    if (value == current) {
                        if (index < sql.length() && sql.charAt(index) == current) { index++; continue; }
                        closed = true; break;
                    }
                }
                if (!closed) return null;
                tokens.add(current == '\'' ? "#value" : "#identifier"); continue;
            }
            int start = index++;
            if (Character.isLetter(current) || current == '_')
                while (index < sql.length() && (Character.isLetterOrDigit(sql.charAt(index)) || sql.charAt(index) == '_')) index++;
            tokens.add(sql.substring(start, index).toUpperCase(Locale.ROOT));
        }
        if (!tokens.isEmpty() && tokens.get(tokens.size() - 1).equals(";")) tokens.remove(tokens.size() - 1);
        return tokens;
    }
}

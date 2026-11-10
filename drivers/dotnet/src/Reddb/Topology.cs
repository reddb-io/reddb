using System.Buffers.Binary;
using System.Text;
using System.Text.Json.Nodes;

namespace Reddb;

/// <summary>Consistency for explicitly declared reads.</summary>
public enum ReadConsistency { Strong, Local, BoundedStaleness }
public sealed record ReadOptions(ReadConsistency Consistency, TimeSpan MaxLag = default);

internal interface ITopologySource
{
    ValueTask<JsonObject?> TopologyAsync(string protocol, CancellationToken cancellationToken);
}

// Contract authority: reddb-wire::topology; shared conformance fixtures pin the bytes.
internal static class TopologyCodec
{
    internal const byte Kind = 0x32;
    internal const int Feature = 2;

    internal static JsonObject? Decode(ReadOnlyMemory<byte> memory)
    {
        byte[] bytes = memory.ToArray();
        if (bytes.Length < 5) throw new InvalidDataException("truncated topology");
        uint length = BinaryPrimitives.ReadUInt32LittleEndian(bytes.AsSpan(1));
        if (length > bytes.Length - 5) throw new InvalidDataException("truncated topology body");
        if (bytes[0] != 1) return null;
        int end = checked(5 + (int)length), offset = 5;
        ReadOnlySpan<byte> Take(int size)
        {
            if (size < 0 || size > end - offset) throw new InvalidDataException("truncated topology field");
            int start = offset;
            offset += size;
            return bytes.AsSpan(start, size);
        }
        uint U32() => BinaryPrimitives.ReadUInt32LittleEndian(Take(4));
        string U64() => BinaryPrimitives.ReadUInt64LittleEndian(Take(8)).ToString(System.Globalization.CultureInfo.InvariantCulture);
        string Text()
        {
            uint size = U32();
            if (size > 4096) throw new InvalidDataException("topology string too long");
            return new UTF8Encoding(false, true).GetString(Take((int)size));
        }
        string epoch = U64();
        var primary = new JsonObject { ["addr"] = Text(), ["region"] = Text() };
        uint count = U32();
        if (count > 256) throw new InvalidDataException("too many topology replicas");
        var replicas = new JsonArray();
        for (int index = 0; index < count; index++)
            replicas.Add(new JsonObject {
                ["addr"] = Text(), ["region"] = Text(), ["healthy"] = Take(1)[0] != 0,
                ["lag_ms"] = U32(), ["last_applied_lsn"] = U64(), ["rebootstrapping"] = false,
            });
        if (end - offset >= count)
            foreach (JsonNode? replica in replicas) replica!["rebootstrapping"] = Take(1)[0] != 0;
        return new JsonObject { ["version"] = 1, ["epoch"] = epoch, ["primary"] = primary, ["replicas"] = replicas };
    }

    internal static string? Address(string? address, string protocol)
    {
        if (string.IsNullOrEmpty(address) || address.Length > 4096) return null;
        if (!address.Contains("://")) address = protocol + "://" + address;
        if (!Uri.TryCreate(address, UriKind.Absolute, out Uri? uri) || uri.Scheme != protocol
            || string.IsNullOrEmpty(uri.Host) || uri.UserInfo.Length != 0
            || uri.Query.Length != 0 || uri.Fragment.Length != 0) return null;
        return uri.AbsoluteUri.TrimEnd('/');
    }
}

internal static class RoutingSql
{
    internal static (string State, bool Read) Policy(string sql)
    {
        var tokens = Tokens(sql);
        if (tokens is null || tokens.Count == 0 || tokens.Contains(";")) return ("session", false);
        string first = tokens[0];
        if (first == "BEGIN" || first == "START" && tokens.Count > 1 && tokens[1] == "TRANSACTION") return ("begin", false);
        if (first is "COMMIT" or "END" or "ROLLBACK")
        {
            if (first == "ROLLBACK" && tokens.Contains("TO")) return ("primary", false);
            return (tokens.Contains("CHAIN") && !tokens.Contains("NO") ? "begin" : "end", false);
        }
        if (new[] { "SET", "RESET", "USE", "PREPARE", "DEALLOCATE", "DECLARE", "LISTEN" }.Contains(first)) return ("session", false);
        var forbidden = new HashSet<string> { "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "INTO", "FOR", "LOCK", "CALL", "EXECUTE", "SET", "QUEUE", "COPY", "[", "]" };
        var functions = new HashSet<string> { "COUNT", "SUM", "MIN", "MAX", "AVG", "LOWER", "UPPER", "LENGTH", "COALESCE", "ABS", "ROUND", "CAST", "EXISTS", "IN", "AS", "FROM", "WHERE", "AND", "OR", "NOT", "SELECT" };
        bool read = first == "SELECT" && !tokens.Any(forbidden.Contains);
        for (int index = 0; index + 1 < tokens.Count && read; index++)
            if (tokens[index + 1] == "(" && (tokens[index] == "#identifier" || char.IsLetter(tokens[index][0]) || tokens[index][0] == '_') && !functions.Contains(tokens[index])) read = false;
        return ("primary", read);
    }

    private static List<string>? Tokens(string sql)
    {
        if (sql.Length > 1_048_576) return null;
        var tokens = new List<string>();
        int index = 0;
        while (index < sql.Length)
        {
            char current = sql[index];
            if (char.IsWhiteSpace(current)) { index++; continue; }
            if (sql.AsSpan(index).StartsWith("--"))
            { int end = sql.IndexOf('\n', index + 2); index = end < 0 ? sql.Length : end + 1; continue; }
            if (sql.AsSpan(index).StartsWith("/*"))
            {
                int depth = 1; index += 2;
                while (index < sql.Length && depth > 0)
                {
                    if (sql.AsSpan(index).StartsWith("/*")) { depth++; index += 2; }
                    else if (sql.AsSpan(index).StartsWith("*/")) { depth--; index += 2; }
                    else index++;
                    if (depth > 64) return null;
                }
                if (depth != 0) return null;
                continue;
            }
            if (current is '\'' or '"' or '`')
            {
                index++; bool closed = false;
                while (index < sql.Length)
                {
                    char value = sql[index++];
                    if (value == '\\') return null;
                    if (value == current)
                    {
                        if (index < sql.Length && sql[index] == current) { index++; continue; }
                        closed = true; break;
                    }
                }
                if (!closed) return null;
                tokens.Add(current == '\'' ? "#value" : "#identifier"); continue;
            }
            int start = index++;
            if (char.IsLetter(current) || current == '_')
                while (index < sql.Length && (char.IsLetterOrDigit(sql[index]) || sql[index] == '_')) index++;
            tokens.Add(sql[start..index].ToUpperInvariant());
        }
        if (tokens.Count > 0 && tokens[^1] == ";") tokens.RemoveAt(tokens.Count - 1);
        return tokens;
    }
}

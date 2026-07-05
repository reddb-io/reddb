// Implementation of the SDK Helper Spec v0.1 — mirrors `drivers/go/helpers.go`.
// JSON parsing is intentionally minimal: only what's needed to walk the engine
// response envelopes (`{ rows: [...], affected: N, result: {...} }`).

#include "reddb/helpers.hpp"

#include <algorithm>
#include <cctype>
#include <cstring>
#include <sstream>

namespace reddb::helpers {

// =========================================================================
// Minimal JSON parser
// =========================================================================
namespace {

struct Parser {
    const std::string& s;
    size_t i = 0;
    explicit Parser(const std::string& src) : s(src) {}

    void skip_ws() {
        while (i < s.size()) {
            char c = s[i];
            if (c == ' ' || c == '\t' || c == '\n' || c == '\r') ++i;
            else break;
        }
    }

    bool eof() const { return i >= s.size(); }
    char peek() const { return s[i]; }

    bool match(const char* lit) {
        size_t n = std::strlen(lit);
        if (i + n > s.size()) return false;
        if (s.compare(i, n, lit) != 0) return false;
        i += n;
        return true;
    }

    bool parse(JsonValue& out) {
        skip_ws();
        if (eof()) return false;
        char c = peek();
        if (c == '{') return parse_object(out);
        if (c == '[') return parse_array(out);
        if (c == '"') return parse_string(out);
        if (c == 't' || c == 'f') return parse_bool(out);
        if (c == 'n') return parse_null(out);
        return parse_number(out);
    }

    bool parse_object(JsonValue& out) {
        ++i; // {
        JsonObject obj;
        skip_ws();
        if (!eof() && peek() == '}') { ++i; out = JsonValue::make_object(std::move(obj)); return true; }
        while (true) {
            skip_ws();
            if (eof() || peek() != '"') return false;
            JsonValue key;
            if (!parse_string(key)) return false;
            skip_ws();
            if (eof() || peek() != ':') return false;
            ++i;
            JsonValue val;
            if (!parse(val)) return false;
            obj.emplace(key.as_string(), std::move(val));
            skip_ws();
            if (eof()) return false;
            if (peek() == ',') { ++i; continue; }
            if (peek() == '}') { ++i; break; }
            return false;
        }
        out = JsonValue::make_object(std::move(obj));
        return true;
    }

    bool parse_array(JsonValue& out) {
        ++i; // [
        JsonArray arr;
        skip_ws();
        if (!eof() && peek() == ']') { ++i; out = JsonValue::make_array(std::move(arr)); return true; }
        while (true) {
            JsonValue v;
            if (!parse(v)) return false;
            arr.push_back(std::move(v));
            skip_ws();
            if (eof()) return false;
            if (peek() == ',') { ++i; continue; }
            if (peek() == ']') { ++i; break; }
            return false;
        }
        out = JsonValue::make_array(std::move(arr));
        return true;
    }

    bool parse_string(JsonValue& out) {
        if (peek() != '"') return false;
        ++i;
        std::string buf;
        while (!eof()) {
            char c = s[i++];
            if (c == '"') { out = JsonValue::make_string(std::move(buf)); return true; }
            if (c == '\\') {
                if (eof()) return false;
                char e = s[i++];
                switch (e) {
                    case '"': buf += '"'; break;
                    case '\\': buf += '\\'; break;
                    case '/': buf += '/'; break;
                    case 'b': buf += '\b'; break;
                    case 'f': buf += '\f'; break;
                    case 'n': buf += '\n'; break;
                    case 'r': buf += '\r'; break;
                    case 't': buf += '\t'; break;
                    case 'u': {
                        if (i + 4 > s.size()) return false;
                        unsigned code = 0;
                        for (int k = 0; k < 4; ++k) {
                            char h = s[i++];
                            code <<= 4;
                            if (h >= '0' && h <= '9') code |= h - '0';
                            else if (h >= 'a' && h <= 'f') code |= 10 + h - 'a';
                            else if (h >= 'A' && h <= 'F') code |= 10 + h - 'A';
                            else return false;
                        }
                        // emit utf-8 (BMP only; no surrogate pair handling)
                        if (code < 0x80) buf += static_cast<char>(code);
                        else if (code < 0x800) {
                            buf += static_cast<char>(0xC0 | (code >> 6));
                            buf += static_cast<char>(0x80 | (code & 0x3F));
                        } else {
                            buf += static_cast<char>(0xE0 | (code >> 12));
                            buf += static_cast<char>(0x80 | ((code >> 6) & 0x3F));
                            buf += static_cast<char>(0x80 | (code & 0x3F));
                        }
                        break;
                    }
                    default: return false;
                }
            } else {
                buf += c;
            }
        }
        return false;
    }

    bool parse_bool(JsonValue& out) {
        if (match("true")) { out = JsonValue::make_bool(true); return true; }
        if (match("false")) { out = JsonValue::make_bool(false); return true; }
        return false;
    }

    bool parse_null(JsonValue& out) {
        if (match("null")) { out = JsonValue::Null(); return true; }
        return false;
    }

    bool parse_number(JsonValue& out) {
        size_t start = i;
        if (!eof() && peek() == '-') ++i;
        bool is_float = false;
        while (!eof()) {
            char c = peek();
            if (c >= '0' && c <= '9') { ++i; continue; }
            if (c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-') {
                is_float = true; ++i; continue;
            }
            break;
        }
        if (i == start) return false;
        std::string tok = s.substr(start, i - start);
        try {
            if (is_float) {
                out = JsonValue::make_double(std::stod(tok));
            } else {
                out = JsonValue::make_int(std::stoll(tok));
            }
            return true;
        } catch (...) {
            return false;
        }
    }
};

} // namespace

// =========================================================================
// SQL helpers
// =========================================================================
namespace sql {

bool is_ident_char(char c) {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
        || (c >= '0' && c <= '9') || c == '_';
}

bool all_ident_chars(const std::string& s) {
    for (char c : s) if (!is_ident_char(c)) return false;
    return !s.empty();
}

static std::string replace_all(std::string s, const std::string& from, const std::string& to) {
    size_t pos = 0;
    while ((pos = s.find(from, pos)) != std::string::npos) {
        s.replace(pos, from.size(), to);
        pos += to.size();
    }
    return s;
}

static std::string kv_key_segment(const std::string& value) {
    if (!value.empty() && all_ident_chars(value)) return value;
    return "'" + replace_all(value, "'", "''") + "'";
}

std::string kv_path(const std::string& collection, const std::string& key) {
    for (char c : collection) {
        if (!is_ident_char(c)) {
            std::string ch(1, c);
            throw HelperError(HelperError::Code::InvalidArgument,
                "invalid KV collection \"" + collection +
                "\": character \"" + ch + "\" is not supported");
        }
    }
    return collection + "." + kv_key_segment(key);
}

// Naive JSON encoder for our minimal JsonValue.
std::string json_encode(const JsonValue& v) {
    std::ostringstream os;
    switch (v.kind()) {
        case JsonValue::Kind::Null:   os << "null"; break;
        case JsonValue::Kind::Bool:   os << (v.as_bool() ? "true" : "false"); break;
        case JsonValue::Kind::Int:    os << v.as_int(); break;
        case JsonValue::Kind::Double: os << v.as_double(); break;
        case JsonValue::Kind::String: {
            os << '"';
            for (char c : v.as_string()) {
                switch (c) {
                    case '"': os << "\\\""; break;
                    case '\\': os << "\\\\"; break;
                    case '\b': os << "\\b"; break;
                    case '\f': os << "\\f"; break;
                    case '\n': os << "\\n"; break;
                    case '\r': os << "\\r"; break;
                    case '\t': os << "\\t"; break;
                    default:
                        if (static_cast<unsigned char>(c) < 0x20) {
                            char buf[8];
                            std::snprintf(buf, sizeof(buf), "\\u%04x", c);
                            os << buf;
                        } else {
                            os << c;
                        }
                }
            }
            os << '"';
            break;
        }
        case JsonValue::Kind::Array: {
            os << '[';
            bool first = true;
            for (const auto& el : v.as_array()) {
                if (!first) os << ',';
                first = false;
                os << json_encode(el);
            }
            os << ']';
            break;
        }
        case JsonValue::Kind::Object: {
            os << '{';
            bool first = true;
            // keys ordered (std::map already alphabetical) — matches Go
            // map iteration is unordered, but tests check substring of body
            for (const auto& [k, val] : v.as_object()) {
                if (!first) os << ',';
                first = false;
                os << '"';
                for (char c : k) {
                    if (c == '"') os << "\\\"";
                    else if (c == '\\') os << "\\\\";
                    else os << c;
                }
                os << "\":" << json_encode(val);
            }
            os << '}';
            break;
        }
    }
    return os.str();
}

std::string kv_value_literal(const JsonValue& v) {
    switch (v.kind()) {
        case JsonValue::Kind::Null:   return "NULL";
        case JsonValue::Kind::Bool:   return v.as_bool() ? "true" : "false";
        case JsonValue::Kind::String: return "'" + replace_all(v.as_string(), "'", "''") + "'";
        case JsonValue::Kind::Int: {
            std::ostringstream os; os << v.as_int(); return os.str();
        }
        case JsonValue::Kind::Double: {
            std::ostringstream os; os << v.as_double(); return os.str();
        }
        default: {
            std::string enc = json_encode(v);
            return "'" + replace_all(enc, "'", "''") + "'";
        }
    }
}

std::string kv_tag_literal(const std::string& tag) {
    return "'" + replace_all(tag, "'", "''") + "'";
}

std::string queue_value_literal(const JsonValue& v) {
    switch (v.kind()) {
        case JsonValue::Kind::Null:   return "NULL";
        case JsonValue::Kind::Bool:   return v.as_bool() ? "true" : "false";
        case JsonValue::Kind::String: return "'" + replace_all(v.as_string(), "'", "''") + "'";
        case JsonValue::Kind::Int: {
            std::ostringstream os; os << v.as_int(); return os.str();
        }
        case JsonValue::Kind::Double: {
            std::ostringstream os; os << v.as_double(); return os.str();
        }
        default: return json_encode(v);
    }
}

std::string value_literal(const JsonValue& v) { return kv_value_literal(v); }

std::string json_literal(const JsonValue& v) {
    std::string enc = json_encode(v);
    return "'" + replace_all(enc, "'", "''") + "'";
}

// ADR 0067 (#1709): a document body is written as an inline strict-JSON
// literal (no surrounding quotes) — the quoted-string coercion is removed.
std::string json_inline_literal(const JsonValue& v) {
    return json_encode(v);
}

std::string identifier(const std::string& value) {
    if (!value.empty() && all_ident_chars(value)) return value;
    return "\"" + replace_all(value, "\"", "\"\"") + "\"";
}

std::string identifier_path(const std::string& value) {
    if (value.find('.') == std::string::npos) return identifier(value);
    std::string out;
    size_t start = 0;
    for (size_t k = 0; k <= value.size(); ++k) {
        if (k == value.size() || value[k] == '.') {
            if (!out.empty()) out += '.';
            out += identifier(value.substr(start, k - start));
            start = k + 1;
        }
    }
    return out;
}

void assert_identifier(const std::string& value, const std::string& label) {
    if (value.empty() || !all_ident_chars(value)) {
        throw HelperError(HelperError::Code::InvalidArgument,
            "invalid " + label + " \"" + value + "\": must match [A-Za-z0-9_]+");
    }
}

int normalize_limit(int value) {
    if (value == 0) return 100;
    if (value < 0) throw HelperError(HelperError::Code::InvalidArgument,
        "limit must be a positive integer");
    return value;
}

// --- response parsing ---

std::optional<JsonObject> decode_body(const std::string& body) {
    if (body.empty()) return std::nullopt;
    Parser p(body);
    JsonValue v;
    if (!p.parse(v) || !v.is_object()) return std::nullopt;
    return v.as_object();
}

static int64_t affected_from_map(const JsonObject& obj) {
    auto it = obj.find("affected");
    if (it == obj.end()) return 0;
    if (it->second.is_int()) return it->second.as_int();
    if (it->second.is_double()) return static_cast<int64_t>(it->second.as_double());
    return 0;
}

std::pair<std::optional<JsonObject>, int64_t> first_row(const std::string& body) {
    auto obj = decode_body(body);
    if (!obj) return {std::nullopt, 0};
    int64_t affected = affected_from_map(*obj);
    const JsonArray* rows = nullptr;
    {
        auto it = obj->find("rows");
        if (it != obj->end() && it->second.is_array()) rows = &it->second.as_array();
    }
    if (!rows || rows->empty()) {
        auto rit = obj->find("result");
        if (rit != obj->end() && rit->second.is_object()) {
            const JsonObject& nested = rit->second.as_object();
            auto rr = nested.find("rows");
            if (rr != nested.end() && rr->second.is_array()) rows = &rr->second.as_array();
            if (affected == 0) affected = affected_from_map(nested);
        }
    }
    if (!rows || rows->empty()) return {std::nullopt, affected};
    if (!(*rows)[0].is_object()) return {std::nullopt, affected};
    return {(*rows)[0].as_object(), affected};
}

std::vector<JsonObject> all_rows(const std::string& body) {
    auto obj = decode_body(body);
    if (!obj) return {};
    const JsonArray* rows = nullptr;
    auto it = obj->find("rows");
    if (it != obj->end() && it->second.is_array()) rows = &it->second.as_array();
    if (!rows) {
        auto rit = obj->find("result");
        if (rit != obj->end() && rit->second.is_object()) {
            const JsonObject& nested = rit->second.as_object();
            auto rr = nested.find("rows");
            if (rr != nested.end() && rr->second.is_array()) rows = &rr->second.as_array();
        }
    }
    if (!rows) return {};
    std::vector<JsonObject> out;
    out.reserve(rows->size());
    for (const auto& r : *rows) {
        if (r.is_object()) out.push_back(r.as_object());
    }
    return out;
}

int64_t affected_from_body(const std::string& body) {
    auto obj = decode_body(body);
    if (!obj) return 0;
    int64_t direct = affected_from_map(*obj);
    if (direct > 0) return direct;
    auto rit = obj->find("result");
    if (rit != obj->end() && rit->second.is_object()) {
        return affected_from_map(rit->second.as_object());
    }
    return 0;
}

std::optional<std::string> rid_string(const JsonValue& value) {
    switch (value.kind()) {
        case JsonValue::Kind::String: return value.as_string();
        case JsonValue::Kind::Int: {
            std::ostringstream os; os << value.as_int(); return os.str();
        }
        case JsonValue::Kind::Double: {
            std::ostringstream os; os << value.as_double(); return os.str();
        }
        default: return std::nullopt;
    }
}

} // namespace sql

// =========================================================================
// Querier adapter
// =========================================================================

namespace {
class ConnQuerier : public IQuerier {
public:
    explicit ConnQuerier(Conn* c) : c_(c) {}
    std::string query(const std::string& sql,
                      const std::vector<std::string>& positional) override {
        if (positional.empty()) return c_->query(sql);
        std::vector<Value> vals;
        vals.reserve(positional.size());
        for (const auto& p : positional) vals.emplace_back(Value(p));
        return c_->query(sql, std::span<const Value>(vals));
    }
private:
    Conn* c_;
};
} // namespace

std::shared_ptr<IQuerier> make_querier(Conn* conn) {
    return std::make_shared<ConnQuerier>(conn);
}

// =========================================================================
// Document client
// =========================================================================

InsertResult DocumentClient::insert(const std::string& collection,
                                    const JsonObject& document) {
    ensure_collection(collection);
    JsonValue doc = JsonValue::make_object(document);
    std::ostringstream os;
    os << "INSERT INTO " << sql::identifier_path(collection)
       << " DOCUMENT VALUES (" << sql::json_inline_literal(doc) << ") RETURNING *";
    std::string body = q_->query(os.str(), {});
    auto [row, affected] = sql::first_row(body);
    if (!row) {
        throw HelperError(HelperError::Code::InvalidResponse,
            "documents.insert expected one returned item with rid");
    }
    auto rid_it = row->find("rid");
    if (rid_it == row->end() || rid_it->second.is_null()) {
        throw HelperError(HelperError::Code::InvalidResponse,
            "documents.insert expected one returned item with rid");
    }
    auto rid_opt = sql::rid_string(rid_it->second);
    if (!rid_opt) {
        throw HelperError(HelperError::Code::InvalidResponse,
            "documents.insert expected one returned item with rid");
    }
    InsertResult res;
    res.affected = affected == 0 ? 1 : affected;
    res.rid = *rid_opt;
    res.item = *row;
    return res;
}

JsonObject DocumentClient::get(const std::string& collection, const std::string& rid) {
    std::ostringstream os;
    os << "SELECT * FROM " << sql::identifier_path(collection) << " WHERE rid = $1 LIMIT 1";
    std::string body = q_->query(os.str(), {rid});
    auto [row, _] = sql::first_row(body);
    if (!row) {
        throw HelperError(HelperError::Code::NotFound,
            "document \"" + rid + "\" was not found");
    }
    return *row;
}

ListResult DocumentClient::list(const std::string& collection, const ListOptions& opts) {
    int limit = sql::normalize_limit(opts.limit);
    std::string order = opts.order_by.empty() ? "rid ASC" : opts.order_by;
    std::string where = opts.filter.empty() ? "" : (" WHERE " + opts.filter);
    std::ostringstream os;
    os << "SELECT * FROM " << sql::identifier_path(collection)
       << where << " ORDER BY " << order << " LIMIT " << limit;
    std::string body = q_->query(os.str(), {});
    ListResult res;
    res.items = sql::all_rows(body);
    return res;
}

JsonObject DocumentClient::patch(const std::string& collection, const std::string& rid,
                                 const JsonObject& patch) {
    if (patch.empty()) return get(collection, rid);
    std::ostringstream parts;
    bool first = true;
    for (const auto& [k, v] : patch) {
        if (k.find('/') != std::string::npos) {
            throw HelperError(HelperError::Code::InvalidArgument,
                "documents.patch currently accepts top-level document fields");
        }
        if (!first) parts << ", ";
        first = false;
        parts << sql::identifier(k) << " = " << sql::value_literal(v);
    }
    std::ostringstream os;
    os << "UPDATE " << sql::identifier_path(collection) << " SET "
       << parts.str() << " WHERE rid = $1 RETURNING *";
    std::string body = q_->query(os.str(), {rid});
    auto [row, _] = sql::first_row(body);
    if (!row) {
        throw HelperError(HelperError::Code::NotFound,
            "document \"" + rid + "\" was not found");
    }
    return *row;
}

DeleteResult DocumentClient::del(const std::string& collection, const std::string& rid) {
    std::ostringstream os;
    os << "DELETE FROM " << sql::identifier_path(collection) << " WHERE rid = $1";
    std::string body = q_->query(os.str(), {rid});
    return DeleteResult{sql::affected_from_body(body)};
}

void DocumentClient::ensure_collection(const std::string& collection) {
    try {
        q_->query("CREATE DOCUMENT " + sql::identifier_path(collection), {});
    } catch (const std::exception& e) {
        std::string msg = e.what();
        if (msg.find("already exists") == std::string::npos) throw;
    }
}

// =========================================================================
// KV client
// =========================================================================

void KvClient::put(const std::string& key, const JsonValue& value, const SetOptions& opts) {
    const std::string& coll = opts.collection.empty() ? collection_ : opts.collection;
    std::string lit = sql::kv_value_literal(value);
    std::string expire;
    if (opts.expire_ms > 0) {
        std::ostringstream os; os << " EXPIRE " << opts.expire_ms << " ms"; expire = os.str();
    }
    std::string tag_clause;
    if (!opts.tags.empty()) {
        std::ostringstream os;
        os << " TAGS [";
        for (size_t i = 0; i < opts.tags.size(); ++i) {
            if (i) os << ", ";
            os << sql::kv_tag_literal(opts.tags[i]);
        }
        os << "]";
        tag_clause = os.str();
    }
    std::string path = sql::kv_path(coll, key);
    q_->query("KV PUT " + path + " = " + lit + expire + tag_clause, {});
}

JsonValue KvClient::get(const std::string& key, const std::string& collection) {
    const std::string& coll = collection.empty() ? collection_ : collection;
    std::string path = sql::kv_path(coll, key);
    std::string body = q_->query("KV GET " + path, {});
    auto [row, _] = sql::first_row(body);
    if (!row) return JsonValue::Null();
    auto it = row->find("value");
    if (it == row->end()) return JsonValue::Null();
    return it->second;
}

ExistsResult KvClient::exists(const std::string& key, const std::string& collection) {
    JsonValue v = get(key, collection);
    return ExistsResult{!v.is_null()};
}

DeleteResult KvClient::del(const std::string& key, const std::string& collection) {
    const std::string& coll = collection.empty() ? collection_ : collection;
    std::string path = sql::kv_path(coll, key);
    std::string body = q_->query("KV DELETE " + path, {});
    return DeleteResult{sql::affected_from_body(body)};
}

ListResult KvClient::list(const ListOpts& opts) {
    const std::string& coll = opts.collection.empty() ? collection_ : opts.collection;
    int limit = sql::normalize_limit(opts.limit);
    std::ostringstream os;
    os << "SELECT key, value FROM " << sql::identifier(coll)
       << " ORDER BY key ASC LIMIT " << limit;
    std::string body = q_->query(os.str(), {});
    ListResult res;
    res.items = sql::all_rows(body);
    if (!opts.prefix.empty()) {
        std::vector<JsonObject> filtered;
        for (auto& r : res.items) {
            auto it = r.find("key");
            if (it != r.end() && it->second.is_string()) {
                const std::string& k = it->second.as_string();
                if (k.rfind(opts.prefix, 0) == 0) filtered.push_back(std::move(r));
            }
        }
        res.items = std::move(filtered);
    }
    return res;
}

// =========================================================================
// Queue client
// =========================================================================

QueuePushResult QueueClient::push(const std::string& queue, const JsonValue& value,
                                  const PushOptions& opts) {
    sql::assert_identifier(queue, "queue name");
    std::string lit = sql::queue_value_literal(value);
    std::string priority;
    if (opts.priority.has_value()) {
        std::ostringstream os; os << " PRIORITY " << *opts.priority; priority = os.str();
    }
    std::string body = q_->query(
        "QUEUE PUSH " + sql::identifier(queue) + " " + lit + priority, {});
    QueuePushResult res;
    res.affected = sql::affected_from_body(body);
    if (res.affected == 0) res.affected = 1;
    auto [row, _] = sql::first_row(body);
    if (row) {
        auto it = row->find("rid");
        if (it != row->end()) {
            auto rid = sql::rid_string(it->second);
            if (rid) res.rid = *rid;
        }
    }
    return res;
}

std::vector<JsonValue> QueueClient::pop(const std::string& queue, std::optional<int> count) {
    return fetch("POP", queue, count);
}

std::vector<JsonValue> QueueClient::peek(const std::string& queue, std::optional<int> count) {
    return fetch("PEEK", queue, count);
}

std::vector<JsonValue> QueueClient::fetch(const char* verb, const std::string& queue,
                                          std::optional<int> count) {
    sql::assert_identifier(queue, "queue name");
    std::string suffix;
    if (count.has_value()) {
        if (*count < 0) {
            throw HelperError(HelperError::Code::InvalidArgument,
                "queue count must be a non-negative integer");
        }
        std::ostringstream os; os << " COUNT " << *count; suffix = os.str();
    }
    std::string body = q_->query(
        std::string("QUEUE ") + verb + " " + sql::identifier(queue) + suffix, {});
    auto rows = sql::all_rows(body);
    std::vector<JsonValue> out;
    out.reserve(rows.size());
    for (auto& r : rows) {
        auto it = r.find("payload");
        out.push_back(it == r.end() ? JsonValue::Null() : it->second);
    }
    return out;
}

int64_t QueueClient::len(const std::string& queue) {
    sql::assert_identifier(queue, "queue name");
    std::string body = q_->query("QUEUE LEN " + sql::identifier(queue), {});
    auto [row, _] = sql::first_row(body);
    if (!row) return 0;
    auto it = row->find("len");
    if (it == row->end()) return 0;
    if (it->second.is_int()) return it->second.as_int();
    if (it->second.is_double()) return static_cast<int64_t>(it->second.as_double());
    return 0;
}

DeleteResult QueueClient::purge(const std::string& queue) {
    sql::assert_identifier(queue, "queue name");
    std::string body = q_->query("QUEUE PURGE " + sql::identifier(queue), {});
    return DeleteResult{sql::affected_from_body(body)};
}

std::vector<JsonValue> QueueClient::read_wait(const std::string& queue,
                                              const std::string& consumer,
                                              int64_t wait_ms,
                                              const ReadWaitOptions& opts) {
    sql::assert_identifier(queue, "queue name");
    sql::assert_identifier(consumer, "consumer name");
    if (wait_ms < 0) {
        throw HelperError(HelperError::Code::InvalidArgument,
            "queue read_wait requires a non-negative wait_ms (no infinite wait)");
    }
    std::string group_clause;
    if (opts.group && !opts.group->empty()) {
        sql::assert_identifier(*opts.group, "group name");
        group_clause = " GROUP " + sql::identifier(*opts.group);
    }
    std::string count_clause;
    if (opts.count) {
        if (*opts.count < 0) {
            throw HelperError(HelperError::Code::InvalidArgument,
                "queue count must be a non-negative integer");
        }
        count_clause = " COUNT " + std::to_string(*opts.count);
    }
    std::string stmt = "QUEUE READ " + sql::identifier(queue) + group_clause
        + " CONSUMER " + sql::identifier(consumer) + count_clause
        + " WAIT " + std::to_string(wait_ms) + "ms";
    std::string body = q_->query(stmt, {});
    auto rows = sql::all_rows(body);
    std::vector<JsonValue> out;
    out.reserve(rows.size());
    for (auto& r : rows) {
        auto it = r.find("payload");
        out.push_back(it != r.end() ? it->second : JsonValue{});
    }
    return out;
}

} // namespace reddb::helpers

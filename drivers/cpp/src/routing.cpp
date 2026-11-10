#include "routing.hpp"
#include "reddb/helpers.hpp"
#include <algorithm>
#include <cctype>
#include <map>
#include <mutex>
#include <set>

namespace reddb::routing {
namespace {
[[noreturn]] void malformed() { throw RedDBError(ErrorCode::Protocol, "invalid topology payload"); }
struct Reader {
    std::span<const uint8_t> bytes;
    size_t offset = 0;
    uint64_t integer(size_t size) {
        if (size > bytes.size() - offset) malformed();
        uint64_t value = 0;
        for (size_t index = 0; index < size; ++index) value |= uint64_t(bytes[offset++]) << (index * 8);
        return value;
    }
    std::string text() {
        auto size = integer(4);
        if (size > 4096 || size > bytes.size() - offset) malformed();
        std::string result(bytes.begin() + offset, bytes.begin() + offset + size);
        offset += static_cast<size_t>(size);
        // UTF-8 is part of the canonical string contract.
        size_t index = 0;
        while (index < result.size()) {
            auto first = static_cast<uint8_t>(result[index++]);
            if (first < 0x80) continue;
            uint32_t value; size_t count;
            if (first >= 0xc2 && first <= 0xdf) { value = first & 31; count = 1; }
            else if (first >= 0xe0 && first <= 0xef) { value = first & 15; count = 2; }
            else if (first >= 0xf0 && first <= 0xf4) { value = first & 7; count = 3; }
            else malformed();
            if (count > result.size() - index) malformed();
            for (size_t next = 0; next < count; ++next) {
                auto byte = static_cast<uint8_t>(result[index++]);
                if ((byte & 0xc0) != 0x80) malformed();
                value = (value << 6) | (byte & 63);
            }
            if ((count == 1 && value < 0x80) || (count == 2 && value < 0x800)
                || (count == 3 && value < 0x10000) || value > 0x10ffff || (value >= 0xd800 && value <= 0xdfff)) malformed();
        }
        return result;
    }
};
bool letter(char value) {
    return (value >= 'a' && value <= 'z') || (value >= 'A' && value <= 'Z') || value == '_';
}
std::optional<std::vector<std::string>> tokens(std::string_view sql) {
    if (sql.size() > 1048576) return {};
    std::vector<std::string> result;
    size_t index = 0;
    while (index < sql.size()) {
        char current = sql[index];
        if (std::isspace(static_cast<unsigned char>(current))) { ++index; continue; }
        if (sql.substr(index).starts_with("--")) {
            auto end = sql.find('\n', index + 2); index = end == sql.npos ? sql.size() : end + 1; continue;
        }
        if (sql.substr(index).starts_with("/*")) {
            size_t depth = 1; index += 2;
            while (index < sql.size() && depth != 0) {
                if (sql.substr(index).starts_with("/*")) { ++depth; index += 2; }
                else if (sql.substr(index).starts_with("*/")) { --depth; index += 2; }
                else ++index;
                if (depth > 64) return {};
            }
            if (depth != 0) return {};
            continue;
        }
        if (current == '\'' || current == '"' || current == '`') {
            ++index; bool closed = false;
            while (index < sql.size()) {
                char value = sql[index++];
                if (value == '\\') return {};
                if (value == current) {
                    if (index < sql.size() && sql[index] == current) { ++index; continue; }
                    closed = true; break;
                }
            }
            if (!closed) return {};
            result.emplace_back(current == '\'' ? "#value" : "#identifier"); continue;
        }
        if (static_cast<unsigned char>(current) > 127) return {};
        size_t start = index++;
        if (letter(current)) while (index < sql.size() && (letter(sql[index]) || (sql[index] >= '0' && sql[index] <= '9'))) ++index;
        std::string token(sql.substr(start, index - start));
        std::transform(token.begin(), token.end(), token.begin(), [](unsigned char value) { return static_cast<char>(std::toupper(value)); });
        result.push_back(std::move(token));
    }
    if (!result.empty() && result.back() == ";") result.pop_back();
    return result;
}
const helpers::JsonValue* field(const helpers::JsonValue& object, const std::string& key) {
    const auto* value = object.find(key); if (!value) malformed(); return value;
}
std::string text(const helpers::JsonValue& object, const std::string& key) {
    const auto* value = field(object, key); if (!value->is_string()) malformed(); return value->as_string();
}
}

std::optional<Topology> decode_topology(std::span<const uint8_t> bytes) {
    if (bytes.size() < 5) malformed();
    Reader header{bytes}; auto version = header.integer(1), length = header.integer(4);
    if (length > bytes.size() - 5) malformed();
    if (version != 1) return {};
    Reader reader{bytes.subspan(5, static_cast<size_t>(length))};
    Topology topology;
    topology.epoch = reader.integer(8);
    topology.primary = {reader.text(), reader.text()};
    auto count = reader.integer(4);
    if (count > 256) malformed();
    for (uint64_t index = 0; index < count; ++index) {
        TopologyReplica replica;
        replica.endpoint = {reader.text(), reader.text()}; replica.healthy = reader.integer(1) != 0;
        replica.lag_ms = static_cast<uint32_t>(reader.integer(4)); replica.last_applied_lsn = reader.integer(8);
        topology.replicas.push_back(std::move(replica));
    }
    if (reader.bytes.size() - reader.offset >= count)
        for (auto& replica : topology.replicas) replica.rebootstrapping = reader.integer(1) != 0;
    return topology;
}
std::optional<Topology> decode_topology_json(const std::string& json) {
    auto value = helpers::parse_json_bounded(json);
    if (field(value, "version")->as_int() != 1) return {};
    Topology topology;
    topology.epoch = std::stoull(text(value, "epoch"));
    auto primary = field(value, "primary");
    topology.primary = {text(*primary, "addr"), text(*primary, "region")};
    auto replicas = field(value, "replicas");
    if (!replicas->is_array() || replicas->as_array().size() > 256) malformed();
    for (auto& node : replicas->as_array()) {
        TopologyReplica replica;
        replica.endpoint = {text(node, "addr"), text(node, "region")};
        auto healthy = field(node, "healthy");
        if (!healthy->is_bool()) malformed();
        replica.healthy = healthy->as_bool();
        auto lag = field(node, "lag_ms");
        if (!lag->is_int() || lag->as_int() < 0 || lag->as_int() > UINT32_MAX) malformed();
        replica.lag_ms = static_cast<uint32_t>(lag->as_int());
        replica.last_applied_lsn = std::stoull(text(node, "last_applied_lsn"));
        auto rebuilding = node.find("rebootstrapping");
        replica.rebootstrapping = rebuilding && rebuilding->is_bool() && rebuilding->as_bool();
        topology.replicas.push_back(std::move(replica));
    }
    return topology;
}
std::string endpoint(const std::string& address, const std::string& protocol) {
    if (address.empty() || address.size() > 4096) return {};
    auto value = address.find("://") == address.npos ? protocol + "://" + address : address;
    if (!value.starts_with(protocol + "://") || value.find_first_of("@?#") != value.npos) return {};
    try { auto parsed = parse_uri(value); if (parsed.host.empty()) return {}; }
    catch (const RedDBError&) { return {}; }
    while (value.ends_with('/')) value.pop_back();
    return value;
}
std::pair<std::string, bool> sql_policy(std::string_view sql) {
    auto parsed = tokens(sql);
    if (!parsed || parsed->empty()) return {"session", false};
    auto& words = *parsed;
    auto has = [&](const std::string& value) { return std::find(words.begin(), words.end(), value) != words.end(); };
    if (has(";")) return {"session", false};
    const auto& first = words.front();
    if (first == "BEGIN" || (first == "START" && words.size() > 1 && words[1] == "TRANSACTION")) return {"begin", false};
    if (first == "COMMIT" || first == "END" || first == "ROLLBACK") {
        if (first == "ROLLBACK" && has("TO")) return {"primary", false};
        return {has("CHAIN") && !has("NO") ? "begin" : "end", false};
    }
    const std::set<std::string> session{"SET", "RESET", "USE", "PREPARE", "DEALLOCATE", "DECLARE", "LISTEN"};
    if (session.contains(first)) return {"session", false};
    const std::set<std::string> forbidden{"INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "INTO", "FOR", "LOCK", "CALL", "EXECUTE", "SET", "QUEUE", "COPY", "[", "]"};
    const std::set<std::string> functions{"COUNT", "SUM", "MIN", "MAX", "AVG", "LOWER", "UPPER", "LENGTH", "COALESCE", "ABS", "ROUND", "CAST", "EXISTS", "IN", "AS", "FROM", "WHERE", "AND", "OR", "NOT", "SELECT"};
    bool read = first == "SELECT";
    for (size_t index = 0; index < words.size(); ++index) {
        if (forbidden.contains(words[index])) read = false;
        if (index + 1 < words.size() && words[index + 1] == "(" && (words[index] == "#identifier" || letter(words[index][0])) && !functions.contains(words[index])) read = false;
    }
    return {"primary", read};
}

namespace {
class RoutedConn final : public Conn {
    std::string seed_, primary_, protocol_, pinned_;
    bool force_primary_, session_ = false, broken_ = false, closed_ = false;
    std::map<std::string, std::unique_ptr<Conn>> clients_;
    std::vector<TopologyReplica> replicas_;
    size_t next_ = 0;
    std::chrono::steady_clock::time_point refresh_at_{};
    std::function<std::unique_ptr<Conn>(const std::string&)> dial_;
    std::recursive_mutex mutex_;
    Conn& node(const std::string& address) {
        auto found = clients_.find(address); if (found != clients_.end()) return *found->second;
        auto client = dial_(address); client->ping();
        auto& result = *client; clients_[address] = std::move(client); return result;
    }
    void refresh() {
        if (!pinned_.empty() || closed_) return;
        auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
        refresh_at_ = std::chrono::steady_clock::now() + std::chrono::seconds(30);
        std::vector<std::string> candidates{primary_, seed_};
        for (const auto& replica : replicas_) candidates.push_back(replica.endpoint.addr);
        for (size_t index = 0; index < candidates.size() && index < 258 && std::chrono::steady_clock::now() < deadline; ++index) {
            try {
                auto control = dial_(candidates[index]); auto topology = control->topology(protocol_); control->close();
                if (!topology) continue;
                auto primary = endpoint(topology->primary.addr, protocol_); if (primary.empty()) continue;
                if (std::find(candidates.begin(), candidates.end(), primary) == candidates.end()) candidates.push_back(primary);
                node(primary); std::vector<TopologyReplica> ready;
                for (auto replica : topology->replicas) {
                    if (std::chrono::steady_clock::now() >= deadline) break;
                    replica.endpoint.addr = endpoint(replica.endpoint.addr, protocol_);
                    if (replica.endpoint.addr.empty() || replica.endpoint.addr == primary || !replica.healthy || replica.rebootstrapping) continue;
                    try { node(replica.endpoint.addr); ready.push_back(std::move(replica)); }
                    catch (const RedDBError&) {}
                }
                primary_ = std::move(primary); replicas_ = std::move(ready);
                std::set<std::string> keep{seed_, primary_};
                for (const auto& replica : replicas_) keep.insert(replica.endpoint.addr);
                for (auto it = clients_.begin(); it != clients_.end();) {
                    if (!keep.contains(it->first)) { it->second->close(); it = clients_.erase(it); } else ++it;
                }
                if (primary_ == candidates[index]) return;
            } catch (const RedDBError& error) { if (error.code() == ErrorCode::AuthRefused) throw; }
        }
    }
    template<typename F> auto invoke(F operation, std::string_view sql = {}, bool read = false, const ReadOptions* options = nullptr) {
        std::lock_guard guard(mutex_);
        if (closed_ || broken_) throw RedDBError(ErrorCode::Network, "connection is closed");
        if (options && (options->lag_max.count() < 0 || (options->consistency != ReadConsistency::Strong && options->consistency != ReadConsistency::Local && options->consistency != ReadConsistency::BoundedStaleness))) throw RedDBError(ErrorCode::ConsistencyUnsupported, "unsupported read consistency");
        if (pinned_.empty() && std::chrono::steady_clock::now() >= refresh_at_) refresh();
        auto [state, safe] = sql.empty() ? std::pair<std::string, bool>{"primary", false} : sql_policy(sql);
        read = read || (options && safe);
        std::vector<std::string> eligible;
        if (!force_primary_ && (!options || options->consistency != ReadConsistency::Strong))
            for (auto& replica : replicas_) if (!options || options->consistency != ReadConsistency::BoundedStaleness || std::chrono::milliseconds(replica.lag_ms) <= options->lag_max) eligible.push_back(replica.endpoint.addr);
        std::string address = !pinned_.empty() ? pinned_ : read && !eligible.empty() ? eligible[next_++ % eligible.size()] : primary_;
        if (state == "begin" || state == "session") { pinned_ = address; session_ = session_ || state == "session"; }
        try {
            auto result = operation(node(address));
            if (state == "end" && !session_) pinned_.clear();
            return result;
        } catch (const RedDBError& error) {
            if (error.code() != ErrorCode::Network && error.code() != ErrorCode::Tls) throw;
            refresh_at_ = {};
            if (auto found = clients_.find(address); found != clients_.end()) { found->second->close(); clients_.erase(found); }
            if (!pinned_.empty()) broken_ = true;
            if (!pinned_.empty() || !read) throw;
            refresh(); return operation(node(primary_));
        }
    }
public:
    RoutedConn(std::string seed, std::unique_ptr<Conn> initial, std::string protocol, bool force_primary, decltype(dial_) dial)
        : seed_(seed), primary_(seed), protocol_(std::move(protocol)), force_primary_(force_primary), dial_(std::move(dial)) {
        clients_[seed] = std::move(initial); refresh();
    }
    std::string query(const std::string& sql) override { return invoke([&](Conn& c) { return c.query(sql); }, sql); }
    std::string query(std::string_view sql, std::span<const Value> params) override { return invoke([&](Conn& c) { return c.query(sql, params); }, sql); }
    std::string query_with_options(std::string_view sql, ReadOptions options, std::span<const Value> params) override { return invoke([&](Conn& c) { return c.query(sql, params); }, sql, false, &options); }
    std::string insert(const std::string& col, const std::string& body) override { return invoke([&](Conn& c) { return c.insert(col, body); }); }
    std::string bulk_insert(const std::string& col, const std::vector<std::string>& rows) override { return invoke([&](Conn& c) { return c.bulk_insert(col, rows); }); }
    std::string get(const std::string& col, const std::string& id) override { return invoke([&](Conn& c) { return c.get(col, id); }, {}, true); }
    std::string del(const std::string& col, const std::string& id) override { return invoke([&](Conn& c) { return c.del(col, id); }); }
    void ping() override { invoke([](Conn& c) { c.ping(); return true; }); }
    void close() override { std::lock_guard guard(mutex_); closed_ = true; for (auto& [_, client] : clients_) client->close(); clients_.clear(); }
    ~RoutedConn() override { try { close(); } catch (...) {} }
};
}
std::unique_ptr<Conn> connect(std::string seed, std::unique_ptr<Conn> initial, std::string protocol, bool force_primary, std::function<std::unique_ptr<Conn>(const std::string&)> dial) {
    return std::make_unique<RoutedConn>(std::move(seed), std::move(initial), std::move(protocol), force_primary, std::move(dial));
}
} // namespace reddb::routing

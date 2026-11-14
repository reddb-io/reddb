// SDK Helper Spec v0.1 — rich helper surface on top of the transport.
// Mirrors `drivers/go/helpers.go` 1:1: `documents`, `kv`, `queue`
// namespaces, the same envelopes (InsertResult/DeleteResult/...), and
// the same typed errors (InvalidArgument / NotFound / InvalidResponse).
//
// All helpers build SQL strings; the same wire request works across
// RedWire and HTTP. The transport contract is `IQuerier` — `Conn`
// satisfies it via `make_querier(conn)`.

#pragma once

#include "reddb/errors.hpp"
#include "reddb/reddb.hpp"

#include <atomic>
#include <cstdint>
#include <exception>
#include <functional>
#include <map>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <thread>
#include <utility>
#include <variant>
#include <vector>

namespace reddb::helpers {

class HelperError : public std::runtime_error {
public:
    enum class Code { InvalidArgument, NotFound, InvalidResponse };
    HelperError(Code code, std::string message)
        : std::runtime_error(std::move(message)), code_(code) {}
    Code code() const noexcept { return code_; }

private:
    Code code_;
};

// --- Minimal JSON value used by envelope parsing ----------------------------

class JsonValue;
using JsonObject = std::map<std::string, JsonValue>;
using JsonArray = std::vector<JsonValue>;

class JsonValue {
public:
    enum class Kind { Null, Bool, Int, Double, String, Array, Object };

    JsonValue() : kind_(Kind::Null) {}
    static JsonValue Null() { return {}; }
    static JsonValue make_bool(bool v) { JsonValue j; j.kind_ = Kind::Bool; j.b_ = v; return j; }
    static JsonValue make_int(int64_t v) { JsonValue j; j.kind_ = Kind::Int; j.i_ = v; return j; }
    static JsonValue make_double(double v) { JsonValue j; j.kind_ = Kind::Double; j.d_ = v; return j; }
    static JsonValue make_string(std::string v) { JsonValue j; j.kind_ = Kind::String; j.s_ = std::move(v); return j; }
    static JsonValue make_array(JsonArray v) {
        JsonValue j;
        j.kind_ = Kind::Array;
        j.arr_ = std::make_shared<JsonArray>(std::move(v));
        return j;
    }
    static JsonValue make_object(JsonObject v) {
        JsonValue j;
        j.kind_ = Kind::Object;
        j.obj_ = std::make_shared<JsonObject>(std::move(v));
        return j;
    }

    Kind kind() const noexcept { return kind_; }
    bool is_null() const noexcept { return kind_ == Kind::Null; }
    bool is_object() const noexcept { return kind_ == Kind::Object; }
    bool is_array() const noexcept { return kind_ == Kind::Array; }
    bool is_string() const noexcept { return kind_ == Kind::String; }
    bool is_int() const noexcept { return kind_ == Kind::Int; }
    bool is_double() const noexcept { return kind_ == Kind::Double; }
    bool is_number() const noexcept { return is_int() || is_double(); }
    bool is_bool() const noexcept { return kind_ == Kind::Bool; }

    bool as_bool() const { return b_; }
    int64_t as_int() const { return kind_ == Kind::Double ? static_cast<int64_t>(d_) : i_; }
    double as_double() const { return kind_ == Kind::Double ? d_ : static_cast<double>(i_); }
    const std::string& as_string() const { return s_; }
    const JsonArray& as_array() const { return *arr_; }
    const JsonObject& as_object() const { return *obj_; }

    const JsonValue* find(const std::string& key) const {
        if (!is_object()) return nullptr;
        auto it = obj_->find(key);
        return it == obj_->end() ? nullptr : &it->second;
    }

private:
    Kind kind_;
    bool b_ = false;
    int64_t i_ = 0;
    double d_ = 0.0;
    std::string s_;
    std::shared_ptr<JsonArray> arr_;
    std::shared_ptr<JsonObject> obj_;
};

// --- Envelopes --------------------------------------------------------------
JsonValue parse_json_bounded(const std::string& text);

struct InsertResult {
    int64_t affected = 0;
    std::string rid;
    std::optional<JsonObject> item;
};
struct DeleteResult { int64_t affected = 0; };
struct ExistsResult { bool exists = false; };
struct ListResult {
    std::vector<JsonObject> items;
    std::optional<std::string> next_cursor;
};
struct QueuePushResult {
    int64_t affected = 0;
    std::optional<std::string> rid;
};
struct QueueMessage {
    std::string message_id;
    std::string delivery_id;
    JsonValue payload;
    std::string consumer;
    int64_t delivery_count = 0;
    JsonObject raw;
};

class ScopedQueueClient;

// --- Querier ----------------------------------------------------------------

class IQuerier {
public:
    virtual ~IQuerier() = default;
    // Returns raw engine JSON envelope. Args are positional `$N` params; an
    // empty vector means "no params, plain query".
    virtual std::string query(const std::string& sql,
                              const std::vector<std::string>& positional_params) = 0;
};

// Adapter so a `reddb::Conn*` satisfies `IQuerier` for callers that already
// have a connection. Lifetime: caller must keep `conn` alive.
std::shared_ptr<IQuerier> make_querier(Conn* conn);

// --- Namespace clients ------------------------------------------------------

class DocumentClient {
public:
    struct ListOptions {
        ListOptions() : limit(0) {}
        int limit;
        std::string order_by;
        std::string filter;
    };

    explicit DocumentClient(std::shared_ptr<IQuerier> q) : q_(std::move(q)) {}

    InsertResult insert(const std::string& collection, const JsonObject& document);
    JsonObject   get(const std::string& collection, const std::string& rid);
    ListResult   list(const std::string& collection, const ListOptions& opts = ListOptions{});
    JsonObject   patch(const std::string& collection, const std::string& rid,
                       const JsonObject& patch);
    DeleteResult del(const std::string& collection, const std::string& rid);

private:
    void ensure_collection(const std::string& collection);
    std::shared_ptr<IQuerier> q_;
};

class KvClient {
public:
    struct SetOptions {
        SetOptions() : expire_ms(0) {}
        std::string collection;
        std::vector<std::string> tags;
        int64_t expire_ms;
    };
    struct ListOpts {
        ListOpts() : limit(0) {}
        std::string collection;
        int limit;
        std::string prefix;
    };

    KvClient(std::shared_ptr<IQuerier> q, std::string collection)
        : q_(std::move(q)), collection_(std::move(collection)) {}

    void         put(const std::string& key, const JsonValue& value,
                     const SetOptions& opts = SetOptions{});
    void         set(const std::string& key, const JsonValue& value,
                     const SetOptions& opts = SetOptions{}) { put(key, value, opts); }
    JsonValue    get(const std::string& key, const std::string& collection = "");
    ExistsResult exists(const std::string& key, const std::string& collection = "");
    DeleteResult del(const std::string& key, const std::string& collection = "");
    ListResult   list(const ListOpts& opts = ListOpts{});

    const std::string& collection() const noexcept { return collection_; }

private:
    std::shared_ptr<IQuerier> q_;
    std::string collection_;
};

class SecretsClient {
public:
    SecretsClient(std::shared_ptr<IQuerier> q, std::string collection);
    const std::string& collection() const noexcept { return collection_; }
    void put(const std::string& key, const JsonValue& value);
    std::optional<JsonObject> get(const std::string& key);
    std::optional<JsonObject> reveal(const std::string& key);

private:
    std::string path(const std::string& key) const;
    std::optional<JsonObject> read(const char* verb, const std::string& key);
    std::shared_ptr<IQuerier> q_;
    std::string collection_;
};

class QueueClient {
public:
    struct PushOptions { std::optional<int> priority; };

    explicit QueueClient(std::shared_ptr<IQuerier> q) : q_(std::move(q)) {}

    ScopedQueueClient scope(const std::string& name);

    QueuePushResult push(const std::string& queue, const JsonValue& value,
                         const PushOptions& opts = PushOptions{});
    std::vector<JsonValue> pop(const std::string& queue,
                               std::optional<int> count = std::nullopt);
    std::vector<JsonValue> peek(const std::string& queue,
                                std::optional<int> count = std::nullopt);
    int64_t      len(const std::string& queue);
    DeleteResult purge(const std::string& queue);

    struct ReadWaitOptions {
        std::optional<std::string> group;
        std::optional<int> count;
    };

    // Live `QUEUE READ … WAIT <ms>` helper (PRD #718 / #725). Blocks
    // until a message is available for `consumer` on `queue`, the
    // wait budget elapses, or the server cancels. Timeout returns an
    // empty vector — same shape as an empty `pop`. `wait_ms` must be
    // explicit; there is no infinite-wait default.
    std::vector<JsonValue> read_wait(const std::string& queue,
                                     const std::string& consumer,
                                     int64_t wait_ms,
                                     const ReadWaitOptions& opts = ReadWaitOptions{});
    std::vector<QueueMessage> read_wait_entries(
        const std::string& queue,
        const std::string& consumer,
        int64_t wait_ms,
        const ReadWaitOptions& opts = ReadWaitOptions{});
    void ack(const std::string& queue, const std::string& delivery_id);
    void nack(const std::string& queue, const std::string& delivery_id,
              const std::string& delay = "");

private:
    std::vector<JsonValue> fetch(const char* verb, const std::string& queue,
                                 std::optional<int> count);
    std::shared_ptr<IQuerier> q_;
};

class ScopedQueueClient {
public:
    ScopedQueueClient(QueueClient queue, std::string name)
        : queue_(std::move(queue)), name_(std::move(name)) {}

    const std::string& name() const noexcept { return name_; }
    QueuePushResult push(const JsonValue& value,
                         const QueueClient::PushOptions& opts = QueueClient::PushOptions{});
    std::vector<JsonValue> pop(std::optional<int> count = std::nullopt);
    std::vector<JsonValue> peek(std::optional<int> count = std::nullopt);
    int64_t len();
    DeleteResult purge();
    std::vector<QueueMessage> read_wait(
        const std::string& consumer,
        int64_t wait_ms,
        const QueueClient::ReadWaitOptions& opts = QueueClient::ReadWaitOptions{});
    void ack(const std::string& delivery_id);
    void nack(const std::string& delivery_id, const std::string& delay = "");

private:
    QueueClient queue_;
    std::string name_;
};

class QueueConsumer {
public:
    struct Options {
        std::string consumer;
        std::string group;
        int64_t wait_ms = 30000;
        std::string retry_delay;
        std::function<void(const QueueMessage&)> handler;
        std::function<void(std::exception_ptr, const QueueMessage*)> on_error;
    };

    QueueConsumer(ScopedQueueClient queue, Options options);
    ~QueueConsumer();
    QueueConsumer(const QueueConsumer&) = delete;
    QueueConsumer& operator=(const QueueConsumer&) = delete;

    QueueConsumer& start();
    void stop();
    void abort();
    void wait();

private:
    void run();
    void report(std::exception_ptr error, const QueueMessage* message);

    ScopedQueueClient queue_;
    Options options_;
    std::atomic<bool> stopping_{false};
    std::thread thread_;
    std::exception_ptr failure_;
};

class Helpers {
public:
    explicit Helpers(std::shared_ptr<IQuerier> q) : q_(std::move(q)) {}
    static Helpers of(Conn* conn) { return Helpers(make_querier(conn)); }

    DocumentClient documents() const { return DocumentClient(q_); }
    KvClient       kv(const std::string& collection = "kv_default") const {
        return KvClient(q_, collection);
    }
    QueueClient    queue() const { return QueueClient(q_); }
    ScopedQueueClient queue(const std::string& name) const {
        return QueueClient(q_).scope(name);
    }
    SecretsClient secrets(const std::string& collection) const {
        return SecretsClient(q_, collection);
    }

private:
    std::shared_ptr<IQuerier> q_;
};

// --- pure SQL helpers (unit-testable) ---------------------------------------

namespace sql {

std::string kv_path(const std::string& collection, const std::string& key);
std::string kv_value_literal(const JsonValue& value);
std::string kv_tag_literal(const std::string& tag);
std::string queue_value_literal(const JsonValue& value);
std::string value_literal(const JsonValue& value);
std::string json_literal(const JsonValue& value);
std::string json_inline_literal(const JsonValue& value);
std::string identifier(const std::string& value);
std::string identifier_path(const std::string& value);
void        assert_identifier(const std::string& value, const std::string& label);
int         normalize_limit(int value);
bool        is_ident_char(char c);
bool        all_ident_chars(const std::string& s);

// JSON serialization for our minimal JsonValue.
std::string json_encode(const JsonValue& value);

// Response parsing.
std::optional<JsonObject> decode_body(const std::string& body);
std::pair<std::optional<JsonObject>, int64_t> first_row(const std::string& body);
std::vector<JsonObject> all_rows(const std::string& body);
int64_t affected_from_body(const std::string& body);
std::optional<std::string> rid_string(const JsonValue& value);

} // namespace sql

} // namespace reddb::helpers

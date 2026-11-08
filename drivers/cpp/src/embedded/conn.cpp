#include "conn.hpp"

#include "reddb/errors.hpp"
#include "reddb/redwire/value_codec.hpp"

#include <reddb.h>

#include <mutex>
#include <string>
#include <utility>

namespace reddb::embedded {

namespace {

std::string take_ffi_string(char* value) {
    if (value == nullptr) return {};
    std::string text(value);
    reddb_string_free(value);
    return text;
}

class EmbeddedConn final : public Conn {
public:
    explicit EmbeddedConn(const ParsedUri& uri) {
        if (reddb_abi_version() != REDDB_ABI_VERSION) {
            throw RedDBError(ErrorCode::Engine, "incompatible embedded RedDB ABI");
        }
        char* error = nullptr;
        const char* path = uri.path.empty() ? nullptr : uri.path.c_str();
        const reddb_status status = reddb_open(path, &handle_, &error);
        std::string message = take_ffi_string(error);
        if (status != REDDB_OK) {
            throw RedDBError(ErrorCode::Engine,
                             message.empty() ? "failed to open embedded RedDB" : message);
        }
    }

    ~EmbeddedConn() override {
        try {
            close();
        } catch (...) {
        }
    }

    std::string query(const std::string& sql) override {
        std::lock_guard lock(mutex_);
        return invoke("query", redwire::to_rpc_query_params(sql, {}));
    }

    std::string query(std::string_view sql, std::span<const Value> params) override {
        std::lock_guard lock(mutex_);
        return invoke("query", redwire::to_rpc_query_params(sql, params));
    }

    std::string insert(const std::string& collection,
                       const std::string& json_payload) override {
        std::lock_guard lock(mutex_);
        return invoke("insert", "{\"collection\":" + redwire::to_json_string(collection) +
                                    ",\"payload\":" + json_payload + "}");
    }

    std::string bulk_insert(const std::string& collection,
                            const std::vector<std::string>& json_rows) override {
        std::string params = "{\"collection\":" + redwire::to_json_string(collection) +
                             ",\"payloads\":[";
        for (size_t i = 0; i < json_rows.size(); ++i) {
            if (i != 0) params.push_back(',');
            params += json_rows[i];
        }
        params += "]}";
        std::lock_guard lock(mutex_);
        return invoke("bulk_insert", params);
    }

    std::string get(const std::string& collection, const std::string& id) override {
        std::lock_guard lock(mutex_);
        return invoke("get", entity_params(collection, id));
    }

    std::string del(const std::string& collection, const std::string& id) override {
        std::lock_guard lock(mutex_);
        return invoke("delete", entity_params(collection, id));
    }

    void ping() override {
        std::lock_guard lock(mutex_);
        (void)invoke("health", "{}");
    }

    void close() override {
        std::lock_guard lock(mutex_);
        if (handle_ == nullptr) return;
        reddb_handle* handle = std::exchange(handle_, nullptr);
        char* error = nullptr;
        const reddb_status status = reddb_close(handle, &error);
        std::string message = take_ffi_string(error);
        if (status != REDDB_OK) {
            throw RedDBError(ErrorCode::Engine,
                             message.empty() ? "failed to close embedded RedDB" : message);
        }
    }

private:
    std::string invoke(const char* method, const std::string& params) {
        if (handle_ == nullptr) {
            throw RedDBError(ErrorCode::Engine, "embedded RedDB connection is closed");
        }
        char* result = nullptr;
        char* error = nullptr;
        const reddb_status status = reddb_invoke(handle_, method, params.c_str(), &result, &error);
        std::string message = take_ffi_string(error);
        if (status != REDDB_OK) {
            take_ffi_string(result);
            throw RedDBError(ErrorCode::Engine,
                             message.empty() ? "embedded RedDB call failed" : message);
        }
        return take_ffi_string(result);
    }

    static std::string entity_params(const std::string& collection, const std::string& id) {
        return "{\"collection\":" + redwire::to_json_string(collection) +
               ",\"id\":" + redwire::to_json_string(id) + "}";
    }

    std::mutex mutex_;
    reddb_handle* handle_ = nullptr;
};

} // namespace

std::unique_ptr<Conn> connect(const ParsedUri& uri) {
    return std::make_unique<EmbeddedConn>(uri);
}

} // namespace reddb::embedded

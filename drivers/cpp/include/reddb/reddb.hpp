// Top-level entry point. `reddb::connect(uri, opts)` parses the
// URI, dials the right transport (RedWire TCP/TLS or HTTP/HTTPS),
// performs auth, and hands back a `Conn` interface.
//
// Embedded URIs (`red://`, `red:///path`, `memory://`, `file://`)
// throw `EmbeddedUnsupported` — this is a remote-only driver.

#pragma once

#include "reddb/errors.hpp"
#include "reddb/http/client.hpp"
#include "reddb/redwire/conn.hpp"
#include "reddb/url.hpp"

#include <memory>
#include <optional>
#include <string>
#include <vector>

namespace reddb {

struct ConnectOptions {
    // Bearer token / API key. Overrides any token in the URI.
    std::optional<std::string> token;
    // Username/password for SCRAM-SHA-256 (RedWire) or HTTP login.
    std::optional<std::string> username;
    std::optional<std::string> password;
    // OAuth-JWT for RedWire.
    std::optional<std::string> jwt;
    // TLS hardening overrides.
    bool dangerous_accept_invalid_certs = false;
    // Client-name advertised in the v2 handshake.
    std::string client_name = "reddb-cpp/0.1";
};

// Common interface so callers can swap RedWire and HTTP transports
// without changing call sites. All ops return raw JSON strings.
class Conn {
public:
    virtual ~Conn() = default;
    virtual std::string query(const std::string& sql) = 0;
    virtual std::string insert(const std::string& collection,
                               const std::string& json_payload) = 0;
    virtual std::string bulk_insert(const std::string& collection,
                                    const std::vector<std::string>& json_rows) = 0;
    virtual std::string get(const std::string& collection, const std::string& id) = 0;
    virtual std::string del(const std::string& collection, const std::string& id) = 0;
    virtual void ping() = 0;
    virtual void close() = 0;
};

std::unique_ptr<Conn> connect(const std::string& uri,
                              const ConnectOptions& opts = ConnectOptions{});

} // namespace reddb

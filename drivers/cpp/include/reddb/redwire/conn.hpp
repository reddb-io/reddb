// RedWire client over TCP (+ optional TLS via OpenSSL).
//
// One connection, one mutex, monotonic correlation ids. The
// public ops return raw JSON strings — callers can plug their
// favourite parser. JSON serialization is only done internally
// for the handshake payloads.

#pragma once

#include "reddb/redwire/frame.hpp"

#include <cstdint>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <vector>

// Forward declarations to keep openssl out of the public header.
typedef struct ssl_ctx_st SSL_CTX;
typedef struct ssl_st SSL;

namespace reddb::redwire {

constexpr uint8_t MAGIC = 0xFE;
constexpr uint8_t SUPPORTED_VERSION = 0x01;

enum class AuthMethod {
    Anonymous,
    Bearer,
    Scram,
    OAuthJwt,
};

struct AuthOpts {
    AuthMethod method = AuthMethod::Anonymous;
    std::string username;     // for SCRAM
    std::string password;     // for SCRAM
    std::string token;        // for Bearer
    std::string jwt;          // for OAuthJwt
};

struct ConnectOpts {
    std::string host;
    uint16_t port = 5050;
    bool tls = false;
    std::string client_name = "reddb-cpp/0.1";
    AuthOpts auth;
    bool dangerous_accept_invalid_certs = false;
};

// Low-level transport interface — abstract so tests can inject a
// socketpair-based fake without standing up TLS.
class IoStream {
public:
    virtual ~IoStream() = default;
    // Read/write exactly `n` bytes. Throw on EOF / error.
    virtual void read_exact(uint8_t* out, size_t n) = 0;
    virtual void write_all(const uint8_t* data, size_t n) = 0;
    virtual void close() noexcept = 0;
};

// TCP-only stream (POSIX). Used by RedWireConn::connect() when
// `opts.tls == false`.
std::unique_ptr<IoStream> dial_tcp(const std::string& host, uint16_t port);

// TCP+TLS stream. ALPN advertises "redwire/1".
std::unique_ptr<IoStream> dial_tls(const std::string& host, uint16_t port,
                                   bool dangerous_accept_invalid_certs);

// Wrap an existing socket fd in a plain TCP IoStream. Takes
// ownership — closes the fd on destruction. Used by tests.
std::unique_ptr<IoStream> wrap_fd(int fd);

class RedWireConn {
public:
    // Connect, perform handshake + auth. Throws on failure.
    static std::unique_ptr<RedWireConn> connect(const ConnectOpts& opts);

    // Inject a pre-built stream (post-magic-byte). Used by the
    // unit tests and when callers need their own transport.
    static std::unique_ptr<RedWireConn> from_stream(std::unique_ptr<IoStream> stream,
                                                    const ConnectOpts& opts);

    ~RedWireConn();
    RedWireConn(const RedWireConn&) = delete;
    RedWireConn& operator=(const RedWireConn&) = delete;

    // ---- Public ops — return raw JSON strings. -----------------
    std::string query(const std::string& sql);
    std::string insert(const std::string& collection, const std::string& json_payload);
    std::string bulk_insert(const std::string& collection,
                            const std::vector<std::string>& json_rows);
    std::string get(const std::string& collection, const std::string& id);
    std::string del(const std::string& collection, const std::string& id);
    void ping();
    void close();

    // Server features bitmask returned in AuthOk.
    uint32_t server_features() const noexcept { return server_features_; }
    const std::string& session_id() const noexcept { return session_id_; }

private:
    explicit RedWireConn(std::unique_ptr<IoStream> stream);
    void send_magic_and_version();
    void run_handshake(const ConnectOpts& opts);
    void run_scram(const ConnectOpts& opts);

    Frame read_frame();
    void write_frame(const Frame& frame);
    uint64_t next_corr();

    std::unique_ptr<IoStream> stream_;
    std::mutex io_;
    uint64_t next_corr_id_ = 1;
    std::string session_id_;
    uint32_t server_features_ = 0;
    bool closed_ = false;
};

} // namespace reddb::redwire

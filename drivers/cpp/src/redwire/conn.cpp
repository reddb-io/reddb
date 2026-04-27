#include "reddb/redwire/conn.hpp"
#include "reddb/redwire/codec.hpp"
#include "reddb/redwire/scram.hpp"
#include "reddb/errors.hpp"

#include <openssl/err.h>
#include <openssl/ssl.h>

#include <arpa/inet.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>
#include <fcntl.h>

#include <cerrno>
#include <cstring>
#include <sstream>
#include <stdexcept>
#include <utility>

namespace reddb::redwire {

// ---------------------------------------------------------------------------
// Minimal JSON encoder/decoder — only used for the handshake payloads.
// Public ops return raw JSON strings so callers pick their own parser.
// ---------------------------------------------------------------------------
namespace tinyjson {

// Encode a JSON string with proper escapes.
static void escape_string(std::string& out, const std::string& s) {
    out.push_back('"');
    for (char c : s) {
        switch (c) {
            case '"':  out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\b': out += "\\b"; break;
            case '\f': out += "\\f"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (static_cast<unsigned char>(c) < 0x20) {
                    char buf[8];
                    std::snprintf(buf, sizeof(buf), "\\u%04x", c);
                    out += buf;
                } else {
                    out.push_back(c);
                }
        }
    }
    out.push_back('"');
}

// Build the Hello payload. Mirrors `drivers/rust/src/redwire/handshake.rs`.
static std::string build_hello(const std::vector<std::string>& methods,
                               const std::string& client_name) {
    std::string s;
    s += "{\"versions\":[1],\"auth_methods\":[";
    for (size_t i = 0; i < methods.size(); ++i) {
        if (i) s.push_back(',');
        escape_string(s, methods[i]);
    }
    s += "],\"features\":0";
    if (!client_name.empty()) {
        s += ",\"client_name\":";
        escape_string(s, client_name);
    }
    s += "}";
    return s;
}

// Tiny scanner: find a string-valued field at the top of a JSON
// object. Good enough for HelloAck / AuthFail / AuthOk payloads
// which are flat objects with no nested structures we care about.
//
// Returns std::nullopt if the field is missing or not a string.
static std::optional<std::string> find_string_field(const std::string& s, const std::string& key) {
    std::string needle = "\"" + key + "\"";
    size_t pos = s.find(needle);
    if (pos == std::string::npos) return std::nullopt;
    pos += needle.size();
    while (pos < s.size() && (s[pos] == ' ' || s[pos] == '\t' || s[pos] == '\n')) ++pos;
    if (pos >= s.size() || s[pos] != ':') return std::nullopt;
    ++pos;
    while (pos < s.size() && (s[pos] == ' ' || s[pos] == '\t' || s[pos] == '\n')) ++pos;
    if (pos >= s.size() || s[pos] != '"') return std::nullopt;
    ++pos;
    std::string out;
    while (pos < s.size()) {
        char c = s[pos];
        if (c == '"') return out;
        if (c == '\\' && pos + 1 < s.size()) {
            char n = s[pos + 1];
            switch (n) {
                case '"':  out.push_back('"'); break;
                case '\\': out.push_back('\\'); break;
                case '/':  out.push_back('/'); break;
                case 'b':  out.push_back('\b'); break;
                case 'f':  out.push_back('\f'); break;
                case 'n':  out.push_back('\n'); break;
                case 'r':  out.push_back('\r'); break;
                case 't':  out.push_back('\t'); break;
                default:   out.push_back(n);
            }
            pos += 2;
            continue;
        }
        out.push_back(c);
        ++pos;
    }
    return std::nullopt;
}

static std::optional<uint64_t> find_u64_field(const std::string& s, const std::string& key) {
    std::string needle = "\"" + key + "\"";
    size_t pos = s.find(needle);
    if (pos == std::string::npos) return std::nullopt;
    pos += needle.size();
    while (pos < s.size() && (s[pos] == ' ' || s[pos] == '\t' || s[pos] == '\n')) ++pos;
    if (pos >= s.size() || s[pos] != ':') return std::nullopt;
    ++pos;
    while (pos < s.size() && (s[pos] == ' ' || s[pos] == '\t' || s[pos] == '\n')) ++pos;
    uint64_t v = 0;
    bool any = false;
    while (pos < s.size() && s[pos] >= '0' && s[pos] <= '9') {
        v = v * 10 + uint64_t(s[pos] - '0');
        ++pos;
        any = true;
    }
    if (!any) return std::nullopt;
    return v;
}

} // namespace tinyjson

// ---------------------------------------------------------------------------
// IoStream implementations.
// ---------------------------------------------------------------------------

namespace {

class FdStream final : public IoStream {
public:
    explicit FdStream(int fd) : fd_(fd) {}
    ~FdStream() override { close(); }

    void read_exact(uint8_t* out, size_t n) override {
        size_t got = 0;
        while (got < n) {
            ssize_t r = ::recv(fd_, out + got, n - got, 0);
            if (r < 0) {
                if (errno == EINTR) continue;
                throw RedDBError(ErrorCode::Network,
                                 std::string("recv: ") + std::strerror(errno));
            }
            if (r == 0) {
                throw RedDBError(ErrorCode::Network, "connection closed mid-frame");
            }
            got += static_cast<size_t>(r);
        }
    }

    void write_all(const uint8_t* data, size_t n) override {
        size_t sent = 0;
        while (sent < n) {
#ifdef MSG_NOSIGNAL
            ssize_t r = ::send(fd_, data + sent, n - sent, MSG_NOSIGNAL);
#else
            ssize_t r = ::send(fd_, data + sent, n - sent, 0);
#endif
            if (r < 0) {
                if (errno == EINTR) continue;
                if (errno == EPIPE) {
                    throw RedDBError(ErrorCode::Network, "connection closed (EPIPE)");
                }
                throw RedDBError(ErrorCode::Network,
                                 std::string("send: ") + std::strerror(errno));
            }
            sent += static_cast<size_t>(r);
        }
    }

    void close() noexcept override {
        if (fd_ >= 0) {
            ::close(fd_);
            fd_ = -1;
        }
    }

private:
    int fd_;
};

class TlsStream final : public IoStream {
public:
    TlsStream(int fd, SSL_CTX* ctx, SSL* ssl)
        : fd_(fd), ctx_(ctx), ssl_(ssl) {}

    ~TlsStream() override { close(); }

    void read_exact(uint8_t* out, size_t n) override {
        size_t got = 0;
        while (got < n) {
            int r = SSL_read(ssl_, out + got, static_cast<int>(n - got));
            if (r > 0) {
                got += static_cast<size_t>(r);
                continue;
            }
            int err = SSL_get_error(ssl_, r);
            if (err == SSL_ERROR_ZERO_RETURN) {
                throw RedDBError(ErrorCode::Network, "TLS connection closed mid-frame");
            }
            throw RedDBError(ErrorCode::Tls,
                             "SSL_read failed: code " + std::to_string(err));
        }
    }

    void write_all(const uint8_t* data, size_t n) override {
        size_t sent = 0;
        while (sent < n) {
            int r = SSL_write(ssl_, data + sent, static_cast<int>(n - sent));
            if (r > 0) {
                sent += static_cast<size_t>(r);
                continue;
            }
            int err = SSL_get_error(ssl_, r);
            throw RedDBError(ErrorCode::Tls,
                             "SSL_write failed: code " + std::to_string(err));
        }
    }

    void close() noexcept override {
        if (ssl_) {
            SSL_shutdown(ssl_);
            SSL_free(ssl_);
            ssl_ = nullptr;
        }
        if (ctx_) {
            SSL_CTX_free(ctx_);
            ctx_ = nullptr;
        }
        if (fd_ >= 0) {
            ::close(fd_);
            fd_ = -1;
        }
    }

private:
    int fd_;
    SSL_CTX* ctx_;
    SSL* ssl_;
};

int open_tcp_socket(const std::string& host, uint16_t port) {
    addrinfo hints{};
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    addrinfo* res = nullptr;
    std::string svc = std::to_string(port);
    int rc = getaddrinfo(host.c_str(), svc.c_str(), &hints, &res);
    if (rc != 0) {
        throw RedDBError(ErrorCode::Network,
                         "getaddrinfo(" + host + "): " + gai_strerror(rc));
    }
    int fd = -1;
    for (addrinfo* p = res; p; p = p->ai_next) {
        fd = ::socket(p->ai_family, p->ai_socktype, p->ai_protocol);
        if (fd < 0) continue;
        if (::connect(fd, p->ai_addr, p->ai_addrlen) == 0) break;
        ::close(fd);
        fd = -1;
    }
    freeaddrinfo(res);
    if (fd < 0) {
        throw RedDBError(ErrorCode::Network,
                         "could not connect to " + host + ":" + svc);
    }
    int yes = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &yes, sizeof(yes));
    return fd;
}

} // namespace

std::unique_ptr<IoStream> wrap_fd(int fd) {
    return std::unique_ptr<IoStream>(new FdStream(fd));
}

std::unique_ptr<IoStream> dial_tcp(const std::string& host, uint16_t port) {
    return wrap_fd(open_tcp_socket(host, port));
}

std::unique_ptr<IoStream> dial_tls(const std::string& host, uint16_t port,
                                   bool dangerous_accept_invalid_certs) {
    int fd = open_tcp_socket(host, port);
    SSL_CTX* ctx = SSL_CTX_new(TLS_client_method());
    if (!ctx) {
        ::close(fd);
        throw RedDBError(ErrorCode::Tls, "SSL_CTX_new failed");
    }
    SSL_CTX_set_min_proto_version(ctx, TLS1_2_VERSION);
    if (dangerous_accept_invalid_certs) {
        SSL_CTX_set_verify(ctx, SSL_VERIFY_NONE, nullptr);
    } else {
        SSL_CTX_set_verify(ctx, SSL_VERIFY_PEER, nullptr);
        SSL_CTX_set_default_verify_paths(ctx);
    }
    // ALPN: redwire/1
    static const unsigned char alpn[] = {9, 'r', 'e', 'd', 'w', 'i', 'r', 'e', '/', '1'};
    SSL_CTX_set_alpn_protos(ctx, alpn, sizeof(alpn));

    SSL* ssl = SSL_new(ctx);
    if (!ssl) {
        SSL_CTX_free(ctx);
        ::close(fd);
        throw RedDBError(ErrorCode::Tls, "SSL_new failed");
    }
    SSL_set_tlsext_host_name(ssl, host.c_str());
    SSL_set_fd(ssl, fd);
    if (SSL_connect(ssl) != 1) {
        unsigned long e = ERR_peek_last_error();
        char buf[256];
        ERR_error_string_n(e, buf, sizeof(buf));
        SSL_free(ssl);
        SSL_CTX_free(ctx);
        ::close(fd);
        throw RedDBError(ErrorCode::Tls, std::string("TLS handshake failed: ") + buf);
    }
    return std::unique_ptr<IoStream>(new TlsStream(fd, ctx, ssl));
}

// ---------------------------------------------------------------------------
// RedWireConn
// ---------------------------------------------------------------------------

RedWireConn::RedWireConn(std::unique_ptr<IoStream> stream) : stream_(std::move(stream)) {}

RedWireConn::~RedWireConn() {
    try { close(); } catch (...) {}
}

std::unique_ptr<RedWireConn> RedWireConn::connect(const ConnectOpts& opts) {
    std::unique_ptr<IoStream> stream;
    if (opts.tls) {
        stream = dial_tls(opts.host, opts.port, opts.dangerous_accept_invalid_certs);
    } else {
        stream = dial_tcp(opts.host, opts.port);
    }
    auto conn = std::unique_ptr<RedWireConn>(new RedWireConn(std::move(stream)));
    conn->send_magic_and_version();
    conn->run_handshake(opts);
    return conn;
}

std::unique_ptr<RedWireConn> RedWireConn::from_stream(std::unique_ptr<IoStream> stream,
                                                      const ConnectOpts& opts) {
    auto conn = std::unique_ptr<RedWireConn>(new RedWireConn(std::move(stream)));
    conn->send_magic_and_version();
    conn->run_handshake(opts);
    return conn;
}

void RedWireConn::send_magic_and_version() {
    uint8_t bytes[2] = {MAGIC, SUPPORTED_VERSION};
    stream_->write_all(bytes, sizeof(bytes));
}

uint64_t RedWireConn::next_corr() {
    return next_corr_id_++;
}

void RedWireConn::write_frame(const Frame& frame) {
    auto bytes = encode_frame(frame);
    stream_->write_all(bytes.data(), bytes.size());
}

Frame RedWireConn::read_frame() {
    uint8_t header[FRAME_HEADER_SIZE];
    stream_->read_exact(header, FRAME_HEADER_SIZE);
    FrameHeader h = decode_header(header);
    if (h.length < FRAME_HEADER_SIZE || h.length > MAX_FRAME_SIZE) {
        throw RedDBError(ErrorCode::Protocol,
                         "server sent frame with length " + std::to_string(h.length));
    }
    std::vector<uint8_t> buf(h.length);
    std::memcpy(buf.data(), header, FRAME_HEADER_SIZE);
    if (h.length > FRAME_HEADER_SIZE) {
        stream_->read_exact(buf.data() + FRAME_HEADER_SIZE, h.length - FRAME_HEADER_SIZE);
    }
    auto [frame, _n] = decode_frame(buf.data(), buf.size());
    return frame;
}

void RedWireConn::run_handshake(const ConnectOpts& opts) {
    // Build Hello with auth methods we can satisfy.
    std::vector<std::string> methods;
    switch (opts.auth.method) {
        case AuthMethod::Anonymous:
            methods = {"anonymous", "bearer"};
            break;
        case AuthMethod::Bearer:
            methods = {"bearer"};
            break;
        case AuthMethod::Scram:
            methods = {"scram-sha-256"};
            break;
        case AuthMethod::OAuthJwt:
            methods = {"oauth-jwt"};
            break;
    }
    std::string hello_json = tinyjson::build_hello(methods, opts.client_name);
    Frame hello;
    hello.kind = MessageKind::Hello;
    hello.correlation_id = next_corr();
    hello.payload.assign(hello_json.begin(), hello_json.end());
    write_frame(hello);

    Frame ack = read_frame();
    if (ack.kind == MessageKind::AuthFail) {
        std::string body(ack.payload.begin(), ack.payload.end());
        auto reason = tinyjson::find_string_field(body, "reason").value_or("AuthFail at HelloAck");
        throw RedDBError(ErrorCode::AuthRefused, "redwire auth refused: " + reason);
    }
    if (ack.kind != MessageKind::HelloAck) {
        throw RedDBError(ErrorCode::Protocol,
                         "expected HelloAck, got 0x" +
                         std::to_string(static_cast<int>(ack.kind)));
    }
    std::string ack_json(ack.payload.begin(), ack.payload.end());
    auto chosen = tinyjson::find_string_field(ack_json, "auth");
    if (!chosen) {
        throw RedDBError(ErrorCode::Protocol, "HelloAck missing 'auth' field");
    }

    if (*chosen == "anonymous") {
        Frame resp;
        resp.kind = MessageKind::AuthResponse;
        resp.correlation_id = next_corr();
        write_frame(resp);
    } else if (*chosen == "bearer") {
        if (opts.auth.method != AuthMethod::Bearer && opts.auth.token.empty()) {
            throw RedDBError(ErrorCode::AuthRefused,
                             "server demanded bearer auth but no token was supplied");
        }
        std::string body;
        body += "{\"token\":";
        tinyjson::escape_string(body, opts.auth.token);
        body += "}";
        Frame resp;
        resp.kind = MessageKind::AuthResponse;
        resp.correlation_id = next_corr();
        resp.payload.assign(body.begin(), body.end());
        write_frame(resp);
    } else if (*chosen == "oauth-jwt") {
        if (opts.auth.jwt.empty()) {
            throw RedDBError(ErrorCode::AuthRefused,
                             "server demanded oauth-jwt but no JWT was supplied");
        }
        std::string body;
        body += "{\"jwt\":";
        tinyjson::escape_string(body, opts.auth.jwt);
        body += "}";
        Frame resp;
        resp.kind = MessageKind::AuthResponse;
        resp.correlation_id = next_corr();
        resp.payload.assign(body.begin(), body.end());
        write_frame(resp);
    } else if (*chosen == "scram-sha-256") {
        run_scram(opts);
        return;
    } else {
        throw RedDBError(ErrorCode::Protocol,
                         "server picked unsupported auth method: " + *chosen);
    }

    // 1-RTT methods read AuthOk / AuthFail.
    Frame final_frame = read_frame();
    if (final_frame.kind == MessageKind::AuthOk) {
        std::string body(final_frame.payload.begin(), final_frame.payload.end());
        session_id_ = tinyjson::find_string_field(body, "session_id").value_or("");
        server_features_ = static_cast<uint32_t>(
            tinyjson::find_u64_field(body, "features").value_or(0));
        return;
    }
    if (final_frame.kind == MessageKind::AuthFail) {
        std::string body(final_frame.payload.begin(), final_frame.payload.end());
        auto reason = tinyjson::find_string_field(body, "reason").value_or("auth refused");
        throw RedDBError(ErrorCode::AuthRefused, "redwire auth refused: " + reason);
    }
    throw RedDBError(ErrorCode::Protocol,
                     "expected AuthOk/AuthFail, got 0x" +
                     std::to_string(static_cast<int>(final_frame.kind)));
}

void RedWireConn::run_scram(const ConnectOpts& opts) {
    if (opts.auth.username.empty() || opts.auth.password.empty()) {
        throw RedDBError(ErrorCode::AuthRefused,
                         "scram-sha-256 requires username + password");
    }
    // client-first: n,,n=<u>,r=<nonce>
    std::string client_nonce = scram::make_client_nonce();
    std::string client_first_bare = "n=" + opts.auth.username + ",r=" + client_nonce;
    std::string client_first = "n,," + client_first_bare;

    Frame f1;
    f1.kind = MessageKind::AuthResponse;
    f1.correlation_id = next_corr();
    f1.payload.assign(client_first.begin(), client_first.end());
    write_frame(f1);

    Frame sf = read_frame();
    if (sf.kind == MessageKind::AuthFail) {
        std::string body(sf.payload.begin(), sf.payload.end());
        auto reason = tinyjson::find_string_field(body, "reason").value_or("scram refused");
        throw RedDBError(ErrorCode::AuthRefused, "redwire auth refused: " + reason);
    }
    if (sf.kind != MessageKind::AuthRequest) {
        throw RedDBError(ErrorCode::Protocol,
                         "expected AuthRequest (server-first) for SCRAM");
    }
    std::string server_first(sf.payload.begin(), sf.payload.end());

    // Parse r=, s=, i=
    std::string combined_nonce, salt_b64;
    uint32_t iter = 0;
    {
        size_t i = 0;
        while (i < server_first.size()) {
            size_t comma = server_first.find(',', i);
            std::string part = server_first.substr(i, comma == std::string::npos ? std::string::npos : comma - i);
            if (part.rfind("r=", 0) == 0) combined_nonce = part.substr(2);
            else if (part.rfind("s=", 0) == 0) salt_b64 = part.substr(2);
            else if (part.rfind("i=", 0) == 0) {
                try { iter = static_cast<uint32_t>(std::stoul(part.substr(2))); }
                catch (...) { throw RedDBError(ErrorCode::Protocol, "scram iter not numeric"); }
            }
            if (comma == std::string::npos) break;
            i = comma + 1;
        }
    }
    if (combined_nonce.empty() || salt_b64.empty() || iter == 0) {
        throw RedDBError(ErrorCode::Protocol, "scram server-first missing fields");
    }
    if (iter < scram::MIN_ITER) {
        throw RedDBError(ErrorCode::Protocol,
                         "scram iter " + std::to_string(iter) + " below MIN_ITER");
    }
    if (combined_nonce.compare(0, client_nonce.size(), client_nonce) != 0) {
        throw RedDBError(ErrorCode::Protocol, "scram nonce mismatch");
    }
    auto salt = scram::base64_decode(salt_b64);

    // client-final without proof, then proof, then send.
    std::string client_final_no_proof = "c=biws,r=" + combined_nonce;
    std::string auth_message_str = client_first_bare + "," + server_first + "," + client_final_no_proof;
    std::vector<uint8_t> auth_message(auth_message_str.begin(), auth_message_str.end());
    auto proof = scram::client_proof(opts.auth.password, salt, iter, auth_message);
    std::string proof_b64 = scram::base64_encode(proof.data(), proof.size());

    std::string client_final = client_final_no_proof + ",p=" + proof_b64;
    Frame f2;
    f2.kind = MessageKind::AuthResponse;
    f2.correlation_id = next_corr();
    f2.payload.assign(client_final.begin(), client_final.end());
    write_frame(f2);

    Frame ok = read_frame();
    if (ok.kind == MessageKind::AuthFail) {
        std::string body(ok.payload.begin(), ok.payload.end());
        auto reason = tinyjson::find_string_field(body, "reason").value_or("scram refused");
        throw RedDBError(ErrorCode::AuthRefused, "redwire auth refused: " + reason);
    }
    if (ok.kind != MessageKind::AuthOk) {
        throw RedDBError(ErrorCode::Protocol, "expected AuthOk after scram client-final");
    }
    std::string body(ok.payload.begin(), ok.payload.end());
    session_id_ = tinyjson::find_string_field(body, "session_id").value_or("");
    server_features_ = static_cast<uint32_t>(
        tinyjson::find_u64_field(body, "features").value_or(0));

    // Optional: if the server sent a base64 server-signature ("v"
    // field), verify it. Hex32 form ("server_signature") also
    // accepted.
    if (auto v_b64 = tinyjson::find_string_field(body, "v")) {
        auto sig = scram::base64_decode(*v_b64);
        if (!scram::verify_server_signature(opts.auth.password, salt, iter, auth_message, sig)) {
            throw RedDBError(ErrorCode::AuthRefused, "scram: server signature mismatch");
        }
    } else if (auto v_hex = tinyjson::find_string_field(body, "server_signature")) {
        auto sig = scram::hex_decode(*v_hex);
        if (!scram::verify_server_signature(opts.auth.password, salt, iter, auth_message, sig)) {
            throw RedDBError(ErrorCode::AuthRefused, "scram: server signature mismatch");
        }
    }
}

// ---------------------------------------------------------------------------
// Public ops.
// ---------------------------------------------------------------------------

std::string RedWireConn::query(const std::string& sql) {
    std::lock_guard<std::mutex> g(io_);
    Frame req;
    req.kind = MessageKind::Query;
    req.correlation_id = next_corr();
    req.payload.assign(sql.begin(), sql.end());
    write_frame(req);
    Frame resp = read_frame();
    if (resp.kind == MessageKind::Result) {
        return std::string(resp.payload.begin(), resp.payload.end());
    }
    if (resp.kind == MessageKind::Error) {
        throw RedDBError(ErrorCode::Engine,
                         std::string(resp.payload.begin(), resp.payload.end()));
    }
    throw RedDBError(ErrorCode::Protocol, "expected Result/Error after query");
}

std::string RedWireConn::insert(const std::string& collection, const std::string& json_payload) {
    std::lock_guard<std::mutex> g(io_);
    std::string body;
    body += "{\"collection\":";
    tinyjson::escape_string(body, collection);
    body += ",\"payload\":";
    body += json_payload;
    body += "}";
    Frame req;
    req.kind = MessageKind::BulkInsert;
    req.correlation_id = next_corr();
    req.payload.assign(body.begin(), body.end());
    write_frame(req);
    Frame resp = read_frame();
    if (resp.kind == MessageKind::BulkOk) {
        return std::string(resp.payload.begin(), resp.payload.end());
    }
    if (resp.kind == MessageKind::Error) {
        throw RedDBError(ErrorCode::Engine,
                         std::string(resp.payload.begin(), resp.payload.end()));
    }
    throw RedDBError(ErrorCode::Protocol, "expected BulkOk/Error after insert");
}

std::string RedWireConn::bulk_insert(const std::string& collection,
                                     const std::vector<std::string>& json_rows) {
    std::lock_guard<std::mutex> g(io_);
    std::string body;
    body += "{\"collection\":";
    tinyjson::escape_string(body, collection);
    body += ",\"payloads\":[";
    for (size_t i = 0; i < json_rows.size(); ++i) {
        if (i) body.push_back(',');
        body += json_rows[i];
    }
    body += "]}";
    Frame req;
    req.kind = MessageKind::BulkInsert;
    req.correlation_id = next_corr();
    req.payload.assign(body.begin(), body.end());
    write_frame(req);
    Frame resp = read_frame();
    if (resp.kind == MessageKind::BulkOk) {
        return std::string(resp.payload.begin(), resp.payload.end());
    }
    if (resp.kind == MessageKind::Error) {
        throw RedDBError(ErrorCode::Engine,
                         std::string(resp.payload.begin(), resp.payload.end()));
    }
    throw RedDBError(ErrorCode::Protocol, "expected BulkOk/Error after bulk_insert");
}

std::string RedWireConn::get(const std::string& collection, const std::string& id) {
    std::lock_guard<std::mutex> g(io_);
    std::string body;
    body += "{\"collection\":";
    tinyjson::escape_string(body, collection);
    body += ",\"id\":";
    tinyjson::escape_string(body, id);
    body += "}";
    Frame req;
    req.kind = MessageKind::Get;
    req.correlation_id = next_corr();
    req.payload.assign(body.begin(), body.end());
    write_frame(req);
    Frame resp = read_frame();
    if (resp.kind == MessageKind::Result) {
        return std::string(resp.payload.begin(), resp.payload.end());
    }
    if (resp.kind == MessageKind::Error) {
        throw RedDBError(ErrorCode::Engine,
                         std::string(resp.payload.begin(), resp.payload.end()));
    }
    throw RedDBError(ErrorCode::Protocol, "expected Result/Error after get");
}

std::string RedWireConn::del(const std::string& collection, const std::string& id) {
    std::lock_guard<std::mutex> g(io_);
    std::string body;
    body += "{\"collection\":";
    tinyjson::escape_string(body, collection);
    body += ",\"id\":";
    tinyjson::escape_string(body, id);
    body += "}";
    Frame req;
    req.kind = MessageKind::Delete;
    req.correlation_id = next_corr();
    req.payload.assign(body.begin(), body.end());
    write_frame(req);
    Frame resp = read_frame();
    if (resp.kind == MessageKind::DeleteOk) {
        return std::string(resp.payload.begin(), resp.payload.end());
    }
    if (resp.kind == MessageKind::Error) {
        throw RedDBError(ErrorCode::Engine,
                         std::string(resp.payload.begin(), resp.payload.end()));
    }
    throw RedDBError(ErrorCode::Protocol, "expected DeleteOk/Error after delete");
}

void RedWireConn::ping() {
    std::lock_guard<std::mutex> g(io_);
    Frame req;
    req.kind = MessageKind::Ping;
    req.correlation_id = next_corr();
    write_frame(req);
    Frame resp = read_frame();
    if (resp.kind != MessageKind::Pong) {
        throw RedDBError(ErrorCode::Protocol, "expected Pong");
    }
}

void RedWireConn::close() {
    std::lock_guard<std::mutex> g(io_);
    if (closed_) return;
    closed_ = true;
    try {
        Frame bye;
        bye.kind = MessageKind::Bye;
        bye.correlation_id = next_corr();
        write_frame(bye);
    } catch (...) {
        // best-effort
    }
    if (stream_) stream_->close();
}

} // namespace reddb::redwire

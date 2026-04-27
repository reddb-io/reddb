#include "reddb/reddb.hpp"
#include "reddb/errors.hpp"

#include <utility>

namespace reddb {

namespace {

class RedWireConnAdapter final : public Conn {
public:
    explicit RedWireConnAdapter(std::unique_ptr<redwire::RedWireConn> c) : c_(std::move(c)) {}
    std::string query(const std::string& sql) override { return c_->query(sql); }
    std::string insert(const std::string& col, const std::string& body) override { return c_->insert(col, body); }
    std::string bulk_insert(const std::string& col, const std::vector<std::string>& rows) override {
        return c_->bulk_insert(col, rows);
    }
    std::string get(const std::string& col, const std::string& id) override { return c_->get(col, id); }
    std::string del(const std::string& col, const std::string& id) override { return c_->del(col, id); }
    void ping() override { c_->ping(); }
    void close() override { c_->close(); }

private:
    std::unique_ptr<redwire::RedWireConn> c_;
};

class HttpConnAdapter final : public Conn {
public:
    explicit HttpConnAdapter(std::unique_ptr<http::HttpClient> c) : c_(std::move(c)) {}

    std::string query(const std::string& sql) override {
        return c_->query(sql).body;
    }
    std::string insert(const std::string& col, const std::string& body) override {
        return c_->insert(col, body).body;
    }
    std::string bulk_insert(const std::string& col, const std::vector<std::string>& rows) override {
        return c_->bulk_insert(col, rows).body;
    }
    std::string get(const std::string& col, const std::string& id) override {
        return c_->get(col, id).body;
    }
    std::string del(const std::string& col, const std::string& id) override {
        return c_->del(col, id).body;
    }
    void ping() override {
        // HTTP has no native ping — health check is the closest thing.
        (void)c_->health();
    }
    void close() override { /* HTTP is stateless */ }

private:
    std::unique_ptr<http::HttpClient> c_;
};

} // namespace

std::unique_ptr<Conn> connect(const std::string& uri, const ConnectOptions& opts) {
    auto parsed = parse_uri(uri);

    if (parsed.kind == UrlKind::Embedded) {
        throw RedDBError(ErrorCode::EmbeddedUnsupported,
                         "embedded URIs are not supported by the C++ driver");
    }

    if (parsed.kind == UrlKind::Http || parsed.kind == UrlKind::Https) {
#if REDDB_HAS_CURL
        http::HttpOpts hopts;
        hopts.base_url = (parsed.kind == UrlKind::Https ? "https://" : "http://") +
                         parsed.host + ":" + std::to_string(parsed.port);
        if (opts.token) hopts.bearer_token = *opts.token;
        else if (parsed.token) hopts.bearer_token = *parsed.token;
        else if (parsed.api_key) hopts.bearer_token = *parsed.api_key;
        hopts.dangerous_accept_invalid_certs = opts.dangerous_accept_invalid_certs;
        auto h = std::make_unique<http::HttpClient>(std::move(hopts));
        if (!opts.token && !parsed.token && !parsed.api_key && opts.username && opts.password) {
            auto resp = h->login(*opts.username, *opts.password);
            // Rough token extraction: look for "token":"…".
            const std::string& body = resp.body;
            auto p = body.find("\"token\"");
            if (p != std::string::npos) {
                p = body.find('"', p + 7);
                if (p != std::string::npos) {
                    auto end = body.find('"', p + 1);
                    if (end != std::string::npos) {
                        h->set_token(body.substr(p + 1, end - p - 1));
                    }
                }
            }
        }
        return std::unique_ptr<Conn>(new HttpConnAdapter(std::move(h)));
#else
        throw RedDBError(ErrorCode::Network,
                         "HTTP transport disabled at build time (libcurl not found)");
#endif
    }

    // RedWire TCP / TLS
    redwire::ConnectOpts rwopts;
    rwopts.host = parsed.host;
    rwopts.port = parsed.port;
    rwopts.tls = (parsed.kind == UrlKind::Reds);
    rwopts.client_name = opts.client_name;
    rwopts.dangerous_accept_invalid_certs = opts.dangerous_accept_invalid_certs;

    if (opts.token) {
        rwopts.auth.method = redwire::AuthMethod::Bearer;
        rwopts.auth.token = *opts.token;
    } else if (parsed.token) {
        rwopts.auth.method = redwire::AuthMethod::Bearer;
        rwopts.auth.token = *parsed.token;
    } else if (opts.jwt) {
        rwopts.auth.method = redwire::AuthMethod::OAuthJwt;
        rwopts.auth.jwt = *opts.jwt;
    } else if (opts.username && opts.password) {
        rwopts.auth.method = redwire::AuthMethod::Scram;
        rwopts.auth.username = *opts.username;
        rwopts.auth.password = *opts.password;
    } else if (parsed.username && parsed.password) {
        rwopts.auth.method = redwire::AuthMethod::Scram;
        rwopts.auth.username = *parsed.username;
        rwopts.auth.password = *parsed.password;
    } else {
        rwopts.auth.method = redwire::AuthMethod::Anonymous;
    }

    auto rw = redwire::RedWireConn::connect(rwopts);
    return std::unique_ptr<Conn>(new RedWireConnAdapter(std::move(rw)));
}

} // namespace reddb

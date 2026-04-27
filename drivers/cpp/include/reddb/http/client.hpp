// HTTP / HTTPS client. libcurl-backed; mirrors the JS driver's
// `HttpRpcClient`. Public methods return raw response bodies as
// strings — the caller parses JSON.

#pragma once

#include <map>
#include <memory>
#include <optional>
#include <string>
#include <vector>

namespace reddb::http {

struct HttpOpts {
    std::string base_url; // e.g. "https://reddb.example.com:8443"
    std::optional<std::string> bearer_token;
    bool dangerous_accept_invalid_certs = false;
    long connect_timeout_seconds = 10;
};

struct HttpResponse {
    long status = 0;
    std::string body;
    std::map<std::string, std::string> headers;
};

class HttpClient {
public:
    explicit HttpClient(HttpOpts opts);
    ~HttpClient();
    HttpClient(const HttpClient&) = delete;
    HttpClient& operator=(const HttpClient&) = delete;

    void set_token(std::string token);

    // Generic request. `body` may be empty.
    HttpResponse request(const std::string& method, const std::string& path,
                         const std::string& body = std::string(),
                         const std::map<std::string, std::string>& headers = {});

    // ---- Convenience wrappers (mirror drivers/js/src/http.js). ----
    HttpResponse health();
    HttpResponse login(const std::string& username, const std::string& password);
    HttpResponse query(const std::string& sql);
    HttpResponse insert(const std::string& collection, const std::string& json_payload);
    HttpResponse bulk_insert(const std::string& collection,
                             const std::vector<std::string>& json_rows);
    HttpResponse scan(const std::string& collection);
    HttpResponse get(const std::string& collection, const std::string& id);
    HttpResponse del(const std::string& collection, const std::string& id);

private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

std::string url_encode(const std::string& s);

} // namespace reddb::http

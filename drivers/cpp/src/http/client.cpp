#include "reddb/http/client.hpp"
#include "reddb/errors.hpp"

#if REDDB_HAS_CURL
#include <curl/curl.h>
#endif

#include <algorithm>
#include <cstdio>
#include <sstream>

namespace reddb::http {

std::string url_encode(const std::string& s) {
    std::string out;
    out.reserve(s.size());
    auto hex = [](unsigned x) -> char {
        return x < 10 ? char('0' + x) : char('A' + x - 10);
    };
    for (unsigned char c : s) {
        if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') ||
            (c >= '0' && c <= '9') || c == '-' || c == '_' || c == '.' || c == '~') {
            out.push_back(static_cast<char>(c));
        } else {
            out.push_back('%');
            out.push_back(hex((c >> 4) & 0xF));
            out.push_back(hex(c & 0xF));
        }
    }
    return out;
}

#if REDDB_HAS_CURL

namespace {

size_t write_to_string(char* ptr, size_t size, size_t nmemb, void* userdata) {
    auto* s = static_cast<std::string*>(userdata);
    s->append(ptr, size * nmemb);
    return size * nmemb;
}

size_t header_to_map(char* ptr, size_t size, size_t nmemb, void* userdata) {
    auto* m = static_cast<std::map<std::string, std::string>*>(userdata);
    size_t total = size * nmemb;
    std::string line(ptr, total);
    auto colon = line.find(':');
    if (colon != std::string::npos) {
        std::string k = line.substr(0, colon);
        std::string v = line.substr(colon + 1);
        // trim trailing CRLF + leading space
        while (!v.empty() && (v.back() == '\r' || v.back() == '\n')) v.pop_back();
        size_t start = v.find_first_not_of(' ');
        if (start != std::string::npos) v = v.substr(start);
        std::transform(k.begin(), k.end(), k.begin(),
                       [](unsigned char c) { return char(std::tolower(c)); });
        (*m)[k] = v;
    }
    return total;
}

} // namespace

struct HttpClient::Impl {
    HttpOpts opts;
    CURL* easy = nullptr;
};

HttpClient::HttpClient(HttpOpts opts) : impl_(std::make_unique<Impl>()) {
    impl_->opts = std::move(opts);
    // Strip trailing slash from base_url for consistent concatenation.
    while (!impl_->opts.base_url.empty() && impl_->opts.base_url.back() == '/') {
        impl_->opts.base_url.pop_back();
    }
    curl_global_init(CURL_GLOBAL_DEFAULT);
    impl_->easy = curl_easy_init();
    if (!impl_->easy) {
        throw RedDBError(ErrorCode::Network, "curl_easy_init failed");
    }
}

HttpClient::~HttpClient() {
    if (impl_ && impl_->easy) {
        curl_easy_cleanup(impl_->easy);
    }
}

void HttpClient::set_token(std::string token) {
    impl_->opts.bearer_token = std::move(token);
}

HttpResponse HttpClient::request(const std::string& method, const std::string& path,
                                 const std::string& body,
                                 const std::map<std::string, std::string>& headers) {
    CURL* h = impl_->easy;
    curl_easy_reset(h);

    std::string url = impl_->opts.base_url + path;
    HttpResponse out;

    curl_easy_setopt(h, CURLOPT_URL, url.c_str());
    curl_easy_setopt(h, CURLOPT_CUSTOMREQUEST, method.c_str());
    curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, write_to_string);
    curl_easy_setopt(h, CURLOPT_WRITEDATA, &out.body);
    curl_easy_setopt(h, CURLOPT_HEADERFUNCTION, header_to_map);
    curl_easy_setopt(h, CURLOPT_HEADERDATA, &out.headers);
    curl_easy_setopt(h, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(h, CURLOPT_CONNECTTIMEOUT, impl_->opts.connect_timeout_seconds);
    curl_easy_setopt(h, CURLOPT_NOSIGNAL, 1L);

    if (impl_->opts.dangerous_accept_invalid_certs) {
        curl_easy_setopt(h, CURLOPT_SSL_VERIFYPEER, 0L);
        curl_easy_setopt(h, CURLOPT_SSL_VERIFYHOST, 0L);
    }

    curl_slist* slist = nullptr;
    auto add_header = [&](const std::string& k, const std::string& v) {
        std::string line = k + ": " + v;
        slist = curl_slist_append(slist, line.c_str());
    };
    if (!body.empty()) add_header("Content-Type", "application/json");
    if (impl_->opts.bearer_token) {
        add_header("Authorization", "Bearer " + *impl_->opts.bearer_token);
    }
    for (auto& kv : headers) add_header(kv.first, kv.second);
    if (slist) curl_easy_setopt(h, CURLOPT_HTTPHEADER, slist);

    if (!body.empty()) {
        curl_easy_setopt(h, CURLOPT_POSTFIELDS, body.c_str());
        curl_easy_setopt(h, CURLOPT_POSTFIELDSIZE, static_cast<long>(body.size()));
    }

    CURLcode rc = curl_easy_perform(h);
    if (slist) curl_slist_free_all(slist);
    if (rc != CURLE_OK) {
        throw RedDBError(ErrorCode::Network,
                         std::string("curl: ") + curl_easy_strerror(rc));
    }
    long status = 0;
    curl_easy_getinfo(h, CURLINFO_RESPONSE_CODE, &status);
    out.status = status;
    return out;
}

HttpResponse HttpClient::health() { return request("GET", "/admin/health"); }

HttpResponse HttpClient::login(const std::string& username, const std::string& password) {
    std::string body = "{\"username\":\"";
    body += username; // caller is responsible for sane input
    body += "\",\"password\":\"";
    body += password;
    body += "\"}";
    return request("POST", "/auth/login", body);
}

HttpResponse HttpClient::query(const std::string& sql) {
    std::string body = "{\"query\":";
    body.push_back('"');
    for (char c : sql) {
        if (c == '"' || c == '\\') body.push_back('\\');
        body.push_back(c);
    }
    body.push_back('"');
    body += "}";
    return request("POST", "/query", body);
}

HttpResponse HttpClient::insert(const std::string& collection, const std::string& json_payload) {
    return request("POST", "/insert?collection=" + url_encode(collection), json_payload);
}

HttpResponse HttpClient::bulk_insert(const std::string& collection,
                                     const std::vector<std::string>& json_rows) {
    std::string body = "{\"rows\":[";
    for (size_t i = 0; i < json_rows.size(); ++i) {
        if (i) body.push_back(',');
        body += json_rows[i];
    }
    body += "]}";
    return request("POST", "/bulk_insert?collection=" + url_encode(collection), body);
}

HttpResponse HttpClient::scan(const std::string& collection) {
    return request("GET", "/scan?collection=" + url_encode(collection));
}

HttpResponse HttpClient::get(const std::string& collection, const std::string& id) {
    return request("GET", "/get?collection=" + url_encode(collection) +
                          "&id=" + url_encode(id));
}

HttpResponse HttpClient::del(const std::string& collection, const std::string& id) {
    return request("DELETE", "/delete?collection=" + url_encode(collection) +
                              "&id=" + url_encode(id));
}

#else // REDDB_HAS_CURL == 0

struct HttpClient::Impl {};
HttpClient::HttpClient(HttpOpts) : impl_(std::make_unique<Impl>()) {
    throw RedDBError(ErrorCode::Network,
                     "HTTP transport disabled at build time (libcurl not found)");
}
HttpClient::~HttpClient() = default;
void HttpClient::set_token(std::string) {}
HttpResponse HttpClient::request(const std::string&, const std::string&,
                                 const std::string&, const std::map<std::string, std::string>&) {
    throw RedDBError(ErrorCode::Network, "HTTP transport disabled at build time");
}
HttpResponse HttpClient::health() { return request("GET", "/admin/health"); }
HttpResponse HttpClient::login(const std::string&, const std::string&) { return request("POST", ""); }
HttpResponse HttpClient::query(const std::string&) { return request("POST", ""); }
HttpResponse HttpClient::insert(const std::string&, const std::string&) { return request("POST", ""); }
HttpResponse HttpClient::bulk_insert(const std::string&, const std::vector<std::string>&) { return request("POST", ""); }
HttpResponse HttpClient::scan(const std::string&) { return request("GET", ""); }
HttpResponse HttpClient::get(const std::string&, const std::string&) { return request("GET", ""); }
HttpResponse HttpClient::del(const std::string&, const std::string&) { return request("DELETE", ""); }
#endif

} // namespace reddb::http

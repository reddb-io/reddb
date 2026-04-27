#include "reddb/url.hpp"
#include "reddb/errors.hpp"

#include <algorithm>
#include <cctype>
#include <cstring>
#include <sstream>
#include <stdexcept>

namespace reddb {

namespace {

std::string to_lower(std::string s) {
    std::transform(s.begin(), s.end(), s.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return s;
}

bool starts_with(const std::string& s, const char* prefix) {
    size_t n = std::strlen(prefix);
    return s.size() >= n && std::memcmp(s.data(), prefix, n) == 0;
}

// Minimal percent-decode for query strings + userinfo. Tolerates
// invalid escapes by passing them through verbatim.
std::string percent_decode(const std::string& s) {
    std::string out;
    out.reserve(s.size());
    for (size_t i = 0; i < s.size(); ++i) {
        if (s[i] == '%' && i + 2 < s.size()) {
            auto hex = [](char c) -> int {
                if (c >= '0' && c <= '9') return c - '0';
                if (c >= 'a' && c <= 'f') return c - 'a' + 10;
                if (c >= 'A' && c <= 'F') return c - 'A' + 10;
                return -1;
            };
            int hi = hex(s[i + 1]);
            int lo = hex(s[i + 2]);
            if (hi >= 0 && lo >= 0) {
                out.push_back(static_cast<char>((hi << 4) | lo));
                i += 2;
                continue;
            }
        }
        out.push_back(s[i]);
    }
    return out;
}

std::map<std::string, std::string> parse_query(const std::string& q) {
    std::map<std::string, std::string> out;
    if (q.empty()) return out;
    size_t i = 0;
    while (i < q.size()) {
        size_t amp = q.find('&', i);
        std::string pair = q.substr(i, (amp == std::string::npos ? q.size() : amp) - i);
        size_t eq = pair.find('=');
        std::string k = (eq == std::string::npos) ? pair : pair.substr(0, eq);
        std::string v = (eq == std::string::npos) ? std::string() : pair.substr(eq + 1);
        out[percent_decode(k)] = percent_decode(v);
        if (amp == std::string::npos) break;
        i = amp + 1;
    }
    return out;
}

// scheme://[user[:pass]@]host[:port][/path][?query]
struct RawUri {
    std::string scheme;
    std::string user;
    std::string pass;
    std::string host;
    std::string port;
    std::string path;
    std::string query;
};

std::optional<RawUri> raw_parse(const std::string& uri) {
    auto sep = uri.find("://");
    if (sep == std::string::npos) return std::nullopt;
    RawUri r;
    r.scheme = to_lower(uri.substr(0, sep));
    std::string rest = uri.substr(sep + 3);

    // split path/query off the authority
    size_t path_start = rest.find_first_of("/?");
    std::string authority;
    std::string tail;
    if (path_start == std::string::npos) {
        authority = rest;
    } else {
        authority = rest.substr(0, path_start);
        tail = rest.substr(path_start);
    }

    // userinfo
    auto at = authority.find('@');
    std::string hostport;
    if (at != std::string::npos) {
        std::string userinfo = authority.substr(0, at);
        hostport = authority.substr(at + 1);
        auto col = userinfo.find(':');
        if (col != std::string::npos) {
            r.user = userinfo.substr(0, col);
            r.pass = userinfo.substr(col + 1);
        } else {
            r.user = userinfo;
        }
    } else {
        hostport = authority;
    }

    // host:port (no IPv6 literal support here — keep small)
    auto col = hostport.rfind(':');
    if (col != std::string::npos && col != 0) {
        r.host = hostport.substr(0, col);
        r.port = hostport.substr(col + 1);
    } else {
        r.host = hostport;
    }

    // path / query
    if (!tail.empty()) {
        auto q = tail.find('?');
        if (q == std::string::npos) {
            r.path = tail;
        } else {
            r.path = tail.substr(0, q);
            r.query = tail.substr(q + 1);
        }
    }
    return r;
}

UrlKind resolve_kind(const std::string& proto) {
    if (proto.empty() || proto == "red" || proto == "grpc") return UrlKind::Red;
    if (proto == "reds" || proto == "grpcs") return UrlKind::Reds;
    if (proto == "http") return UrlKind::Http;
    if (proto == "https") return UrlKind::Https;
    throw RedDBError(ErrorCode::UnsupportedScheme,
                     "unknown proto='" + proto + "'. Supported: red | reds | grpc | grpcs | http | https");
}

} // namespace

uint16_t default_port_for(UrlKind kind) {
    switch (kind) {
        case UrlKind::Http: return 8080;
        case UrlKind::Https: return 8443;
        case UrlKind::Red: return 5050;
        case UrlKind::Reds: return 5051;
        case UrlKind::Embedded: return 0;
    }
    return 0;
}

ParsedUri parse_uri(const std::string& uri) {
    if (uri.empty()) {
        throw RedDBError(ErrorCode::InvalidUri,
                         "connect() requires a URI string (e.g. 'red://localhost:5050')");
    }

    // ---- Embedded variants → not supported in C++ driver. -------------
    if (uri == "memory://" || uri == "memory:" ||
        uri == "red://" || uri == "red:" || uri == "red:/" ||
        uri == "red://memory" || uri == "red://memory/" ||
        uri == "red://:memory" || uri == "red://:memory:") {
        throw RedDBError(ErrorCode::EmbeddedUnsupported,
                         "embedded URIs are not supported by the C++ driver: '" + uri + "'");
    }
    if (starts_with(uri, "red:///")) {
        throw RedDBError(ErrorCode::EmbeddedUnsupported,
                         "embedded path URIs are not supported by the C++ driver: '" + uri + "'");
    }
    if (starts_with(uri, "file://")) {
        throw RedDBError(ErrorCode::EmbeddedUnsupported,
                         "file:// URIs are not supported by the C++ driver: '" + uri + "'");
    }

    auto raw = raw_parse(uri);
    if (!raw) {
        throw RedDBError(ErrorCode::InvalidUri, "failed to parse URI '" + uri + "'");
    }

    ParsedUri out;
    out.original_uri = uri;
    out.params = parse_query(raw->query);

    if (raw->scheme == "red" || raw->scheme == "reds" ||
        raw->scheme == "grpc" || raw->scheme == "grpcs") {
        // For `red://`, the proto query param can override (matches JS).
        std::string proto = to_lower(out.params.count("proto") ? out.params["proto"] : "");
        if (raw->scheme == "reds" || raw->scheme == "grpcs") {
            // Explicit secure scheme — TLS regardless of proto override.
            out.kind = UrlKind::Reds;
        } else if (!proto.empty()) {
            out.kind = resolve_kind(proto);
        } else {
            out.kind = UrlKind::Red;
        }
    } else if (raw->scheme == "http") {
        out.kind = UrlKind::Http;
    } else if (raw->scheme == "https") {
        out.kind = UrlKind::Https;
    } else {
        throw RedDBError(ErrorCode::UnsupportedScheme,
                         "unsupported URI scheme '" + raw->scheme + "': '" + uri + "'");
    }

    if (raw->host.empty()) {
        throw RedDBError(ErrorCode::InvalidUri, "URI is missing a host: '" + uri + "'");
    }
    out.host = raw->host;

    if (!raw->port.empty()) {
        try {
            int p = std::stoi(raw->port);
            if (p < 0 || p > 65535) throw std::out_of_range("port");
            out.port = static_cast<uint16_t>(p);
        } catch (...) {
            throw RedDBError(ErrorCode::InvalidUri, "invalid port '" + raw->port + "' in '" + uri + "'");
        }
    } else {
        out.port = default_port_for(out.kind);
    }

    if (!raw->path.empty() && raw->path != "/") {
        out.path = raw->path;
    }
    if (!raw->user.empty()) out.username = percent_decode(raw->user);
    if (!raw->pass.empty()) out.password = percent_decode(raw->pass);

    if (out.params.count("token")) out.token = out.params["token"];
    if (out.params.count("apiKey")) out.api_key = out.params["apiKey"];
    else if (out.params.count("api_key")) out.api_key = out.params["api_key"];
    if (out.params.count("loginUrl")) out.login_url = out.params["loginUrl"];
    else if (out.params.count("login_url")) out.login_url = out.params["login_url"];

    return out;
}

} // namespace reddb

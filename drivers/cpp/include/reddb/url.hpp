// `red://` connection-string parser. Mirrors `drivers/js/src/url.js`
// behaviour — same accepted schemes, same default ports.
//
// red://, reds://, http://, https:// are the supported network
// transports. Embedded variants (`red:///path`, `memory://`,
// `file://...`) are classified here; `connect()` decides whether the
// optional embedded engine was linked into this build.

#pragma once

#include <cstdint>
#include <map>
#include <optional>
#include <string>

namespace reddb {

enum class UrlKind {
    Embedded, // red://, red:///path, memory://, file://
    Http,
    Https,
    Red,      // red:// (TCP, no TLS)
    Reds,     // reds:// (TCP + TLS)
};

struct ParsedUri {
    UrlKind kind = UrlKind::Red;
    std::string host;
    uint16_t port = 0;
    std::string path; // only for embedded
    std::optional<std::string> username;
    std::optional<std::string> password;
    std::optional<std::string> token;
    std::optional<std::string> api_key;
    std::optional<std::string> login_url;
    std::map<std::string, std::string> params;
    std::string original_uri;
};

// Parse any URI string. Throws `RedDBError` on failure.
ParsedUri parse_uri(const std::string& uri);

// Default port for a given URL kind. 0 if unknown.
uint16_t default_port_for(UrlKind kind);

} // namespace reddb

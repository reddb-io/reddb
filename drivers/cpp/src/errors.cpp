#include "reddb/errors.hpp"

namespace reddb {

const char* error_code_name(ErrorCode code) noexcept {
    switch (code) {
        case ErrorCode::Network: return "NETWORK";
        case ErrorCode::Protocol: return "PROTOCOL";
        case ErrorCode::InvalidUri: return "INVALID_URI";
        case ErrorCode::UnsupportedScheme: return "UNSUPPORTED_SCHEME";
        case ErrorCode::EmbeddedUnsupported: return "EMBEDDED_UNSUPPORTED";
        case ErrorCode::AuthRefused: return "AUTH_REFUSED";
        case ErrorCode::Engine: return "ENGINE";
        case ErrorCode::NotFound: return "NOT_FOUND";
        case ErrorCode::CompressedButNoZstd: return "COMPRESSED_BUT_NO_ZSTD";
        case ErrorCode::Tls: return "TLS";
        case ErrorCode::Unknown: return "UNKNOWN";
    }
    return "UNKNOWN";
}

} // namespace reddb

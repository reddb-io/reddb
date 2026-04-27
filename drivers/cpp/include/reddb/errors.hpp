// RedDB C++ driver — error types.
//
// One exception type, tagged with an `ErrorCode`. Mirrors the
// JS driver's `RedDBError` and the Rust driver's `ClientError`
// so a single switch on `code` works across drivers.

#pragma once

#include <stdexcept>
#include <string>

namespace reddb {

enum class ErrorCode {
    Network,
    Protocol,
    InvalidUri,
    UnsupportedScheme,
    EmbeddedUnsupported,
    AuthRefused,
    Engine,
    NotFound,
    CompressedButNoZstd,
    Tls,
    Unknown,
};

const char* error_code_name(ErrorCode code) noexcept;

class RedDBError : public std::runtime_error {
public:
    RedDBError(ErrorCode code, std::string message)
        : std::runtime_error(std::move(message)), code_(code) {}

    ErrorCode code() const noexcept { return code_; }

private:
    ErrorCode code_;
};

} // namespace reddb

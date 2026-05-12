#include "reddb/value.hpp"

#include <cctype>

namespace reddb {

namespace {

uint8_t hex_value(char c) {
    if (c >= '0' && c <= '9') return static_cast<uint8_t>(c - '0');
    if (c >= 'a' && c <= 'f') return static_cast<uint8_t>(10 + c - 'a');
    if (c >= 'A' && c <= 'F') return static_cast<uint8_t>(10 + c - 'A');
    throw std::invalid_argument("reddb Value uuid contains non-hex digit");
}

std::string canonical_uuid(std::string_view input) {
    std::string hex;
    hex.reserve(32);
    for (char c : input) {
        if (c == '-') continue;
        hex.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(c))));
    }
    if (hex.size() != 32) {
        throw std::invalid_argument("reddb Value uuid must contain 32 hex digits");
    }
    return hex.substr(0, 8) + "-" + hex.substr(8, 4) + "-" + hex.substr(12, 4) +
           "-" + hex.substr(16, 4) + "-" + hex.substr(20, 12);
}

} // namespace

Value Value::bytes(std::span<const std::byte> value) {
    Bytes out;
    out.data.assign(value.begin(), value.end());
    return Value(std::move(out));
}

Value Value::vector(std::span<const float> value) {
    Vector out;
    out.data.assign(value.begin(), value.end());
    return Value(std::move(out));
}

Value Value::json(std::string_view value) {
    return Value(Json{std::string(value)});
}

Value Value::timestamp_seconds(int64_t epoch_seconds) {
    return Value(Timestamp{epoch_seconds});
}

Value Value::timestamp(std::chrono::system_clock::time_point value) {
    auto seconds = std::chrono::duration_cast<std::chrono::seconds>(value.time_since_epoch());
    return timestamp_seconds(seconds.count());
}

Value Value::uuid(std::string_view value) {
    Uuid out;
    out.canonical = canonical_uuid(value);
    std::string hex;
    hex.reserve(32);
    for (char c : out.canonical) {
        if (c != '-') hex.push_back(c);
    }
    for (size_t i = 0; i < out.bytes.size(); ++i) {
        out.bytes[i] = std::byte((hex_value(hex[2 * i]) << 4) | hex_value(hex[2 * i + 1]));
    }
    return Value(std::move(out));
}

} // namespace reddb

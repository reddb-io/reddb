#include "reddb/redwire/value_codec.hpp"
#include "reddb/errors.hpp"

#include <cmath>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <iomanip>
#include <limits>
#include <sstream>

namespace reddb::redwire {

namespace {

void put_u32_le(std::vector<uint8_t>& out, uint32_t value) {
    out.push_back(static_cast<uint8_t>(value & 0xff));
    out.push_back(static_cast<uint8_t>((value >> 8) & 0xff));
    out.push_back(static_cast<uint8_t>((value >> 16) & 0xff));
    out.push_back(static_cast<uint8_t>((value >> 24) & 0xff));
}

void put_u64_le(std::vector<uint8_t>& out, uint64_t value) {
    for (int i = 0; i < 8; ++i) {
        out.push_back(static_cast<uint8_t>((value >> (i * 8)) & 0xff));
    }
}

std::vector<uint8_t> len_prefixed(uint8_t tag, const uint8_t* bytes, size_t len) {
    if (len > MAX_VALUE_PAYLOAD_LEN) {
        throw RedDBError(ErrorCode::Protocol,
                         "redwire value payload exceeds MAX_VALUE_PAYLOAD_LEN");
    }
    std::vector<uint8_t> out;
    out.reserve(1 + 4 + len);
    out.push_back(tag);
    put_u32_le(out, static_cast<uint32_t>(len));
    out.insert(out.end(), bytes, bytes + len);
    return out;
}

std::string json_escape(std::string_view value) {
    std::string out;
    out.reserve(value.size() + 2);
    out.push_back('"');
    for (unsigned char c : value) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\b': out += "\\b"; break;
            case '\f': out += "\\f"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (c < 0x20) {
                    char buf[7];
                    std::snprintf(buf, sizeof(buf), "\\u%04x", c);
                    out += buf;
                } else {
                    out.push_back(static_cast<char>(c));
                }
        }
    }
    out.push_back('"');
    return out;
}

std::string number_json(double value) {
    if (!std::isfinite(value)) {
        throw RedDBError(ErrorCode::Protocol, "non-finite float parameter cannot be JSON encoded");
    }
    std::ostringstream ss;
    ss << std::setprecision(std::numeric_limits<double>::max_digits10) << value;
    return ss.str();
}

constexpr char BASE64[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

uint32_t byte_u32(std::byte b) {
    return static_cast<uint32_t>(std::to_integer<uint8_t>(b));
}

std::string base64_encode(const std::vector<std::byte>& bytes) {
    std::string out;
    out.reserve(((bytes.size() + 2) / 3) * 4);
    size_t i = 0;
    while (i + 3 <= bytes.size()) {
        uint32_t n = (byte_u32(bytes[i]) << 16) |
                     (byte_u32(bytes[i + 1]) << 8) |
                     byte_u32(bytes[i + 2]);
        out.push_back(BASE64[(n >> 18) & 63]);
        out.push_back(BASE64[(n >> 12) & 63]);
        out.push_back(BASE64[(n >> 6) & 63]);
        out.push_back(BASE64[n & 63]);
        i += 3;
    }
    if (i < bytes.size()) {
        uint32_t n = byte_u32(bytes[i]) << 16;
        if (i + 1 < bytes.size()) n |= byte_u32(bytes[i + 1]) << 8;
        out.push_back(BASE64[(n >> 18) & 63]);
        out.push_back(BASE64[(n >> 12) & 63]);
        out.push_back(i + 1 < bytes.size() ? BASE64[(n >> 6) & 63] : '=');
        out.push_back('=');
    }
    return out;
}

std::string value_to_http_json(const Value& value) {
    return std::visit([](const auto& item) -> std::string {
        using T = std::decay_t<decltype(item)>;
        if constexpr (std::is_same_v<T, Value::Null>) {
            return "null";
        } else if constexpr (std::is_same_v<T, bool>) {
            return item ? "true" : "false";
        } else if constexpr (std::is_same_v<T, int64_t>) {
            return std::to_string(item);
        } else if constexpr (std::is_same_v<T, double>) {
            return number_json(item);
        } else if constexpr (std::is_same_v<T, std::string>) {
            return json_escape(item);
        } else if constexpr (std::is_same_v<T, Value::Bytes>) {
            return "{\"$bytes\":" + json_escape(base64_encode(item.data)) + "}";
        } else if constexpr (std::is_same_v<T, Value::Vector>) {
            std::string out = "[";
            for (size_t i = 0; i < item.data.size(); ++i) {
                if (i) out.push_back(',');
                out += number_json(item.data[i]);
            }
            out.push_back(']');
            return out;
        } else if constexpr (std::is_same_v<T, Value::Json>) {
            return item.text;
        } else if constexpr (std::is_same_v<T, Value::Timestamp>) {
            return std::string("{\"$ts\":") + std::to_string(item.epoch_seconds) + "}";
        } else {
            return "{\"$uuid\":" + json_escape(item.canonical) + "}";
        }
    }, value.storage());
}

} // namespace

std::vector<uint8_t> encode_value(const Value& value) {
    return std::visit([](const auto& item) -> std::vector<uint8_t> {
        using T = std::decay_t<decltype(item)>;
        if constexpr (std::is_same_v<T, Value::Null>) {
            return {TAG_NULL};
        } else if constexpr (std::is_same_v<T, bool>) {
            return {TAG_BOOL, static_cast<uint8_t>(item ? 1 : 0)};
        } else if constexpr (std::is_same_v<T, int64_t>) {
            std::vector<uint8_t> out;
            out.reserve(9);
            out.push_back(TAG_INT);
            put_u64_le(out, static_cast<uint64_t>(item));
            return out;
        } else if constexpr (std::is_same_v<T, double>) {
            uint64_t bits = 0;
            static_assert(sizeof(bits) == sizeof(item));
            std::memcpy(&bits, &item, sizeof(bits));
            std::vector<uint8_t> out;
            out.reserve(9);
            out.push_back(TAG_FLOAT);
            put_u64_le(out, bits);
            return out;
        } else if constexpr (std::is_same_v<T, std::string>) {
            return len_prefixed(TAG_TEXT, reinterpret_cast<const uint8_t*>(item.data()), item.size());
        } else if constexpr (std::is_same_v<T, Value::Bytes>) {
            return len_prefixed(TAG_BYTES, reinterpret_cast<const uint8_t*>(item.data.data()),
                                item.data.size());
        } else if constexpr (std::is_same_v<T, Value::Vector>) {
            size_t byte_len = item.data.size() * sizeof(float);
            if (byte_len > MAX_VALUE_PAYLOAD_LEN) {
                throw RedDBError(ErrorCode::Protocol,
                                 "redwire vector payload exceeds MAX_VALUE_PAYLOAD_LEN");
            }
            std::vector<uint8_t> out;
            out.reserve(1 + 4 + byte_len);
            out.push_back(TAG_VECTOR);
            put_u32_le(out, static_cast<uint32_t>(item.data.size()));
            for (float value : item.data) {
                uint32_t bits = 0;
                static_assert(sizeof(bits) == sizeof(value));
                std::memcpy(&bits, &value, sizeof(bits));
                put_u32_le(out, bits);
            }
            return out;
        } else if constexpr (std::is_same_v<T, Value::Json>) {
            return len_prefixed(TAG_JSON, reinterpret_cast<const uint8_t*>(item.text.data()),
                                item.text.size());
        } else if constexpr (std::is_same_v<T, Value::Timestamp>) {
            std::vector<uint8_t> out;
            out.reserve(9);
            out.push_back(TAG_TIMESTAMP);
            put_u64_le(out, static_cast<uint64_t>(item.epoch_seconds));
            return out;
        } else {
            std::vector<uint8_t> out;
            out.reserve(17);
            out.push_back(TAG_UUID);
            for (std::byte b : item.bytes) out.push_back(std::to_integer<uint8_t>(b));
            return out;
        }
    }, value.storage());
}

std::vector<uint8_t> encode_query_with_params(std::string_view sql,
                                              std::span<const Value> params) {
    if (params.size() > MAX_PARAM_COUNT) {
        throw RedDBError(ErrorCode::Protocol, "redwire param_count exceeds MAX_PARAM_COUNT");
    }
    if (sql.size() > MAX_VALUE_PAYLOAD_LEN) {
        throw RedDBError(ErrorCode::Protocol, "redwire sql_len exceeds MAX_VALUE_PAYLOAD_LEN");
    }

    std::vector<std::vector<uint8_t>> encoded;
    encoded.reserve(params.size());
    size_t total = 4 + sql.size() + 4;
    for (const Value& param : params) {
        encoded.push_back(encode_value(param));
        total += encoded.back().size();
    }

    std::vector<uint8_t> out;
    out.reserve(total);
    put_u32_le(out, static_cast<uint32_t>(sql.size()));
    out.insert(out.end(), sql.begin(), sql.end());
    put_u32_le(out, static_cast<uint32_t>(params.size()));
    for (const auto& param : encoded) out.insert(out.end(), param.begin(), param.end());
    return out;
}

std::string to_http_params_json(std::span<const Value> params) {
    std::string out = "[";
    for (size_t i = 0; i < params.size(); ++i) {
        if (i) out.push_back(',');
        out += value_to_http_json(params[i]);
    }
    out.push_back(']');
    return out;
}

std::string to_http_query_body(std::string_view sql, std::span<const Value> params) {
    std::string body = "{\"query\":";
    body += json_escape(sql);
    if (!params.empty()) {
        body += ",\"params\":";
        body += to_http_params_json(params);
    }
    body.push_back('}');
    return body;
}

} // namespace reddb::redwire

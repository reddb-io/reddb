#include "reddb/redwire/value_codec.hpp"
#include "reddb/errors.hpp"

#include <algorithm>
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

void put_u16_le(std::vector<uint8_t>& out, uint16_t value) {
    out.push_back(static_cast<uint8_t>(value & 0xff));
    out.push_back(static_cast<uint8_t>((value >> 8) & 0xff));
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

std::string hex_encode(std::span<const std::byte> bytes) {
    constexpr char digits[] = "0123456789abcdef";
    std::string out;
    out.reserve(bytes.size() * 2);
    for (std::byte byte : bytes) {
        auto value = std::to_integer<uint8_t>(byte);
        out.push_back(digits[value >> 4]);
        out.push_back(digits[value & 0x0f]);
    }
    return out;
}

std::string biometric_format_name(uint16_t format) {
    switch (format) {
        case 0x0101: return "ISO/IEC 39794-2";
        case 0x0102: return "ISO/IEC 39794-4";
        case 0x0103: return "ISO/IEC 19794-2";
        case 0x0104: return "ISO/IEC 19794-4";
        case 0x0105: return "ANSI INCITS 378";
        case 0x0106: return "ANSI INCITS 381";
        case 0x0107: return "ANSI/NIST-ITL Type-4";
        case 0x0108: return "ANSI/NIST-ITL Type-9";
        case 0x0109: return "CBEFF fingerprint";
        case 0x0201: return "ISO/IEC 39794-5";
        case 0x0202: return "ISO/IEC 19794-5";
        case 0x0203: return "ANSI/NIST-ITL Type-10";
        case 0x0204: return "CBEFF face";
        case 0x0301: return "ISO/IEC 39794-6";
        case 0x0302: return "ISO/IEC 19794-6";
        case 0x0303: return "ANSI/NIST-ITL Type-17";
        case 0x0304: return "CBEFF iris";
        default: throw RedDBError(ErrorCode::Protocol, "unknown biometric format");
    }
}

template <typename T>
std::vector<uint8_t> encode_biometric(uint8_t tag, const T& item) {
    if (item.data.empty() || item.data.size() > MAX_VALUE_PAYLOAD_LEN) {
        throw RedDBError(ErrorCode::Protocol, "invalid biometric payload length");
    }
    std::vector<uint8_t> out;
    out.reserve(1 + 2 + 4 + item.data.size());
    out.push_back(tag);
    put_u16_le(out, item.format);
    put_u32_le(out, static_cast<uint32_t>(item.data.size()));
    for (std::byte byte : item.data) out.push_back(std::to_integer<uint8_t>(byte));
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
        } else if constexpr (std::is_same_v<T, Value::Uuid>) {
            return "{\"$uuid\":" + json_escape(item.canonical) + "}";
        } else if constexpr (std::is_same_v<T, Value::Fingerprint>) {
            return "{\"$biometric\":{\"modality\":\"fingerprint\",\"format\":" +
                   json_escape(biometric_format_name(item.format)) + ",\"data\":" +
                   json_escape(hex_encode(item.data)) + "}}";
        } else if constexpr (std::is_same_v<T, Value::Face>) {
            return "{\"$biometric\":{\"modality\":\"face\",\"format\":" +
                   json_escape(biometric_format_name(item.format)) + ",\"data\":" +
                   json_escape(hex_encode(item.data)) + "}}";
        } else if constexpr (std::is_same_v<T, Value::Iris>) {
            return "{\"$biometric\":{\"modality\":\"iris\",\"format\":" +
                   json_escape(biometric_format_name(item.format)) + ",\"data\":" +
                   json_escape(hex_encode(item.data)) + "}}";
        } else if constexpr (std::is_same_v<T, Value::FeatureVector>) {
            std::string out = "{\"$feature_vector\":{\"space_id\":" +
                              json_escape(hex_encode(item.space_id)) + ",\"values\":[";
            for (size_t i = 0; i < item.values.size(); ++i) {
                if (i) out.push_back(',');
                out += number_json(item.values[i]);
            }
            return out + "]}}";
        } else if constexpr (std::is_same_v<T, Value::BitSignature>) {
            return "{\"$bit_signature\":{\"space_id\":" +
                   json_escape(hex_encode(item.space_id)) + ",\"bits\":" +
                   json_escape(hex_encode(item.bits)) + ",\"mask\":" +
                   json_escape(hex_encode(item.mask)) + "}}";
        } else {
            std::string out = "{\"$landmark_set\":{\"space_id\":" +
                              json_escape(hex_encode(item.space_id)) + ",\"dimensions\":" +
                              std::to_string(item.dimensions) + ",\"pairs\":[";
            for (size_t i = 0; i < item.pairs.size(); ++i) {
                if (i) out.push_back(',');
                out += "{\"left\":[";
                for (size_t axis = 0; axis < item.pairs[i].left.size(); ++axis) {
                    if (axis) out.push_back(',');
                    out += number_json(item.pairs[i].left[axis]);
                }
                out += "],\"right\":[";
                for (size_t axis = 0; axis < item.pairs[i].right.size(); ++axis) {
                    if (axis) out.push_back(',');
                    out += number_json(item.pairs[i].right[axis]);
                }
                out += "]}";
            }
            out += "],\"midline\":[";
            for (size_t i = 0; i < item.midline.size(); ++i) {
                if (i) out.push_back(',');
                out.push_back('[');
                for (size_t axis = 0; axis < item.midline[i].size(); ++axis) {
                    if (axis) out.push_back(',');
                    out += number_json(item.midline[i][axis]);
                }
                out.push_back(']');
            }
            return out + "]}}";
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
        } else if constexpr (std::is_same_v<T, Value::Uuid>) {
            std::vector<uint8_t> out;
            out.reserve(17);
            out.push_back(TAG_UUID);
            for (std::byte b : item.bytes) out.push_back(std::to_integer<uint8_t>(b));
            return out;
        } else if constexpr (std::is_same_v<T, Value::Fingerprint>) {
            return encode_biometric(TAG_FINGERPRINT, item);
        } else if constexpr (std::is_same_v<T, Value::Face>) {
            return encode_biometric(TAG_FACE, item);
        } else if constexpr (std::is_same_v<T, Value::Iris>) {
            return encode_biometric(TAG_IRIS, item);
        } else if constexpr (std::is_same_v<T, Value::FeatureVector>) {
            if (item.values.empty() || item.values.size() > 65'536) {
                throw RedDBError(ErrorCode::Protocol, "invalid feature vector dimension");
            }
            std::vector<uint8_t> out;
            out.reserve(1 + 16 + 4 + item.values.size() * 4);
            out.push_back(TAG_FEATURE_VECTOR);
            for (std::byte byte : item.space_id) out.push_back(std::to_integer<uint8_t>(byte));
            put_u32_le(out, static_cast<uint32_t>(item.values.size()));
            double norm = 0;
            for (float value : item.values) {
                if (!std::isfinite(value)) {
                    throw RedDBError(ErrorCode::Protocol, "feature vector coordinates must be finite");
                }
                norm += static_cast<double>(value) * value;
                uint32_t bits = 0;
                std::memcpy(&bits, &value, sizeof(bits));
                put_u32_le(out, bits);
            }
            if (norm == 0) throw RedDBError(ErrorCode::Protocol, "feature vector norm must be non-zero");
            return out;
        } else if constexpr (std::is_same_v<T, Value::BitSignature>) {
            if (item.bits.empty() || item.bits.size() > (1u << 20) ||
                item.mask.size() != item.bits.size() ||
                std::none_of(item.mask.begin(), item.mask.end(), [](std::byte byte) {
                    return byte != std::byte{0};
                })) {
                throw RedDBError(ErrorCode::Protocol, "invalid bit signature length or mask");
            }
            std::vector<uint8_t> out;
            out.reserve(1 + 16 + 4 + item.bits.size() * 2);
            out.push_back(TAG_BIT_SIGNATURE);
            for (std::byte byte : item.space_id) out.push_back(std::to_integer<uint8_t>(byte));
            put_u32_le(out, static_cast<uint32_t>(item.bits.size()));
            for (std::byte byte : item.bits) out.push_back(std::to_integer<uint8_t>(byte));
            for (std::byte byte : item.mask) out.push_back(std::to_integer<uint8_t>(byte));
            return out;
        } else {
            const size_t point_count = item.pairs.size() * 2 + item.midline.size();
            if ((item.dimensions != 2 && item.dimensions != 3) || item.pairs.size() < 2 ||
                point_count > 4096) {
                throw RedDBError(ErrorCode::Protocol, "invalid landmark geometry");
            }
            std::vector<uint8_t> out;
            out.reserve(1 + 16 + 1 + 8 + point_count * item.dimensions * 4);
            out.push_back(TAG_LANDMARK_SET);
            for (std::byte byte : item.space_id) out.push_back(std::to_integer<uint8_t>(byte));
            out.push_back(item.dimensions);
            put_u32_le(out, static_cast<uint32_t>(item.pairs.size()));
            put_u32_le(out, static_cast<uint32_t>(item.midline.size()));
            const auto put_point = [&](const std::vector<float>& point) {
                if (point.size() != item.dimensions) {
                    throw RedDBError(ErrorCode::Protocol, "invalid landmark geometry");
                }
                for (float coordinate : point) {
                    if (!std::isfinite(coordinate)) {
                        throw RedDBError(ErrorCode::Protocol, "invalid landmark geometry");
                    }
                    uint32_t bits = 0;
                    std::memcpy(&bits, &coordinate, sizeof(bits));
                    put_u32_le(out, bits);
                }
            };
            for (const auto& pair : item.pairs) {
                put_point(pair.left);
                put_point(pair.right);
            }
            for (const auto& point : item.midline) put_point(point);
            return out;
        }
    }, value.storage());
}

bool requires_biometric_capability(const Value& value) noexcept {
    return std::visit([](const auto& item) {
        using T = std::decay_t<decltype(item)>;
        return std::is_same_v<T, Value::Fingerprint> || std::is_same_v<T, Value::Face> ||
               std::is_same_v<T, Value::Iris> || std::is_same_v<T, Value::FeatureVector> ||
               std::is_same_v<T, Value::BitSignature>;
    }, value.storage());
}

bool requires_landmark_capability(const Value& value) noexcept {
    return std::holds_alternative<Value::LandmarkSet>(value.storage());
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

std::string to_json_string(std::string_view value) {
    return json_escape(value);
}

std::string to_rpc_query_params(std::string_view sql, std::span<const Value> params) {
    std::string body = "{\"sql\":";
    body += json_escape(sql);
    if (!params.empty()) {
        body += ",\"params\":";
        body += to_http_params_json(params);
    }
    body.push_back('}');
    return body;
}

} // namespace reddb::redwire

#pragma once

#include "reddb/value.hpp"
#include "reddb/redwire/frame.hpp"

#include <cstdint>
#include <initializer_list>
#include <span>
#include <string>
#include <string_view>
#include <vector>

namespace reddb::redwire {

constexpr uint8_t TAG_NULL = 0x00;
constexpr uint8_t TAG_BOOL = 0x01;
constexpr uint8_t TAG_INT = 0x02;
constexpr uint8_t TAG_FLOAT = 0x03;
constexpr uint8_t TAG_TEXT = 0x04;
constexpr uint8_t TAG_BYTES = 0x05;
constexpr uint8_t TAG_VECTOR = 0x06;
constexpr uint8_t TAG_JSON = 0x07;
constexpr uint8_t TAG_TIMESTAMP = 0x08;
constexpr uint8_t TAG_UUID = 0x09;
constexpr uint8_t TAG_FINGERPRINT = 0x0a;
constexpr uint8_t TAG_FACE = 0x0b;
constexpr uint8_t TAG_IRIS = 0x0c;
constexpr uint8_t TAG_FEATURE_VECTOR = 0x0d;
constexpr uint8_t TAG_BIT_SIGNATURE = 0x0e;

constexpr size_t MAX_PARAM_COUNT = 65'536;
constexpr size_t MAX_VALUE_PAYLOAD_LEN = MAX_FRAME_SIZE;

std::vector<uint8_t> encode_value(const reddb::Value& value);
bool requires_biometric_capability(const reddb::Value& value) noexcept;
std::vector<uint8_t> encode_query_with_params(std::string_view sql,
                                              std::span<const reddb::Value> params);
inline std::vector<uint8_t> encode_query_with_params(
    std::string_view sql,
    std::initializer_list<reddb::Value> params) {
    return encode_query_with_params(sql, std::span<const reddb::Value>(params.begin(), params.size()));
}

std::string to_http_params_json(std::span<const reddb::Value> params);
std::string to_http_query_body(std::string_view sql, std::span<const reddb::Value> params);
std::string to_json_string(std::string_view value);
std::string to_rpc_query_params(std::string_view sql, std::span<const reddb::Value> params);

} // namespace reddb::redwire

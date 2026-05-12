#pragma once

#include "reddb/value.hpp"
#include "reddb/redwire/frame.hpp"

#include <cstdint>
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

constexpr size_t MAX_PARAM_COUNT = 65'536;
constexpr size_t MAX_VALUE_PAYLOAD_LEN = MAX_FRAME_SIZE;

std::vector<uint8_t> encode_value(const reddb::Value& value);
std::vector<uint8_t> encode_query_with_params(std::string_view sql,
                                              std::span<const reddb::Value> params);

std::string to_http_params_json(std::span<const reddb::Value> params);
std::string to_http_query_body(std::string_view sql, std::span<const reddb::Value> params);

} // namespace reddb::redwire

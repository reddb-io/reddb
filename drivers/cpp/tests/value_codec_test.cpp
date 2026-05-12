#include "reddb/value.hpp"
#include "reddb/redwire/value_codec.hpp"

#include <gtest/gtest.h>

#include <array>
#include <cstddef>
#include <cstdint>
#include <span>
#include <string>
#include <vector>

using reddb::Value;
using namespace reddb::redwire;

namespace {

uint8_t b(unsigned value) {
    return static_cast<uint8_t>(value);
}

} // namespace

TEST(ValueCodec, ValueTagTableIsPinned) {
    EXPECT_EQ(TAG_NULL, 0x00);
    EXPECT_EQ(TAG_BOOL, 0x01);
    EXPECT_EQ(TAG_INT, 0x02);
    EXPECT_EQ(TAG_FLOAT, 0x03);
    EXPECT_EQ(TAG_TEXT, 0x04);
    EXPECT_EQ(TAG_BYTES, 0x05);
    EXPECT_EQ(TAG_VECTOR, 0x06);
    EXPECT_EQ(TAG_JSON, 0x07);
    EXPECT_EQ(TAG_TIMESTAMP, 0x08);
    EXPECT_EQ(TAG_UUID, 0x09);
}

TEST(ValueCodec, EncodesScalarValues) {
    EXPECT_EQ(encode_value(Value(std::nullopt)), std::vector<uint8_t>({0x00}));
    EXPECT_EQ(encode_value(Value(true)), std::vector<uint8_t>({0x01, 0x01}));
    EXPECT_EQ(encode_value(Value(false)), std::vector<uint8_t>({0x01, 0x00}));
    EXPECT_EQ(encode_value(Value(1)), std::vector<uint8_t>({0x02, 1, 0, 0, 0, 0, 0, 0, 0}));
    EXPECT_EQ(encode_value(Value(-1)), std::vector<uint8_t>({
        0x02, b(0xff), b(0xff), b(0xff), b(0xff), b(0xff), b(0xff), b(0xff), b(0xff),
    }));
    EXPECT_EQ(encode_value(Value("x")), std::vector<uint8_t>({0x04, 1, 0, 0, 0, 'x'}));
}

TEST(ValueCodec, EncodesBytesTimestampUuidAndJson) {
    std::array<std::byte, 4> bytes = {
        std::byte{0xde}, std::byte{0xad}, std::byte{0xbe}, std::byte{0xef},
    };
    EXPECT_EQ(encode_value(Value::bytes(bytes)), std::vector<uint8_t>({
        0x05, 4, 0, 0, 0, b(0xde), b(0xad), b(0xbe), b(0xef),
    }));

    EXPECT_EQ(encode_value(Value::timestamp_seconds(1'700'000'000)), std::vector<uint8_t>({
        0x08, 0x00, 0xf1, 0x53, 0x65, 0x00, 0x00, 0x00, 0x00,
    }));

    auto uuid = Value::uuid("00112233-4455-6677-8899-aabbccddeeff");
    EXPECT_EQ(encode_value(uuid), std::vector<uint8_t>({
        0x09, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
        b(0x88), b(0x99), b(0xaa), b(0xbb), b(0xcc), b(0xdd), b(0xee), b(0xff),
    }));

    EXPECT_EQ(encode_value(Value::json(R"({"a":1,"b":2})")), std::vector<uint8_t>({
        0x07, 13, 0, 0, 0, '{', '"', 'a', '"', ':', '1', ',', '"', 'b', '"', ':', '2', '}',
    }));
}

TEST(ValueCodec, EncodesVectorFromFloatSpan) {
    std::array<float, 3> vec = {1.0f, 2.0f, -0.5f};
    EXPECT_EQ(encode_value(Value::vector(vec)), std::vector<uint8_t>({
        0x06, 3, 0, 0, 0,
        0x00, 0x00, b(0x80), 0x3f,
        0x00, 0x00, 0x00, 0x40,
        0x00, 0x00, 0x00, b(0xbf),
    }));
}

TEST(ValueCodec, EncodesQueryWithParamsPayload) {
    std::array<Value, 3> params = {Value(42), Value("x"), Value(std::nullopt)};
    auto encoded = encode_query_with_params("Q", params);
    EXPECT_EQ(encoded, std::vector<uint8_t>({
        1, 0, 0, 0, 'Q',
        3, 0, 0, 0,
        0x02, 42, 0, 0, 0, 0, 0, 0, 0,
        0x04, 1, 0, 0, 0, 'x',
        0x00,
    }));
}

TEST(ValueCodec, HttpParamsUseJsonEnvelopesForTaggedValues) {
    std::array<std::byte, 2> bytes = {std::byte{'h'}, std::byte{'i'}};
    std::array<float, 2> vec = {1.0f, 2.0f};
    std::array<Value, 10> params = {
        Value(std::nullopt),
        Value(true),
        Value(42),
        Value(1.5),
        Value("txt"),
        Value::bytes(bytes),
        Value::vector(vec),
        Value::json(R"({"a":1,"b":2})"),
        Value::timestamp_seconds(1'700'000'000),
        Value::uuid("00112233-4455-6677-8899-aabbccddeeff"),
    };

    EXPECT_EQ(to_http_params_json(params),
              R"([null,true,42,1.5,"txt",{"$bytes":"aGk="},[1,2],{"a":1,"b":2},{"$ts":1700000000},{"$uuid":"00112233-4455-6677-8899-aabbccddeeff"}])");
}

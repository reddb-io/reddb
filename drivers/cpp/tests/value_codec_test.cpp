#include "reddb/value.hpp"
#include "reddb/redwire/value_codec.hpp"

#include <gtest/gtest.h>

#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <limits>
#include <regex>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

using reddb::Value;
using namespace reddb::redwire;

namespace {

uint8_t b(unsigned value) {
    return static_cast<uint8_t>(value);
}

std::string read_fixture_manifest() {
    const std::array<const char*, 5> paths = {
        "crates/reddb-wire/tests/fixtures/params/manifest.json",
        "../../crates/reddb-wire/tests/fixtures/params/manifest.json",
        "../../../crates/reddb-wire/tests/fixtures/params/manifest.json",
        "../../../../crates/reddb-wire/tests/fixtures/params/manifest.json",
        "../../../../../crates/reddb-wire/tests/fixtures/params/manifest.json",
    };

    for (const char* path : paths) {
        std::ifstream file(path);
        if (file) {
            return std::string(std::istreambuf_iterator<char>(file),
                               std::istreambuf_iterator<char>());
        }
    }
    throw std::runtime_error("parameter fixture manifest not found");
}

std::string manifest_hex(const std::string& manifest, const std::string& name) {
    std::regex pattern("\"name\"\\s*:\\s*\"" + name +
                       "\"[\\s\\S]*?\"redwire_hex\"\\s*:\\s*\"([0-9a-f]+)\"");
    std::smatch match;
    if (!std::regex_search(manifest, match, pattern)) {
        throw std::runtime_error("fixture not found: " + name);
    }
    return match[1].str();
}

std::vector<uint8_t> hex_to_bytes(const std::string& hex) {
    if (hex.size() % 2 != 0) throw std::runtime_error("odd hex length");
    std::vector<uint8_t> out;
    out.reserve(hex.size() / 2);
    for (size_t i = 0; i < hex.size(); i += 2) {
        out.push_back(static_cast<uint8_t>(std::stoul(hex.substr(i, 2), nullptr, 16)));
    }
    return out;
}

double double_from_bits(uint64_t bits) {
    double value = 0.0;
    std::memcpy(&value, &bits, sizeof(value));
    return value;
}

Value fixture_value(const std::string& name) {
    if (name == "null") return Value(std::nullopt);
    if (name == "bool_true") return Value(true);
    if (name == "bool_false") return Value(false);
    if (name == "int_min") return Value(std::numeric_limits<int64_t>::min());
    if (name == "int_max") return Value(std::numeric_limits<int64_t>::max());
    if (name == "int_42") return Value(42);
    if (name == "float_nan") return Value(double_from_bits(0x7ff8000000000000ULL));
    if (name == "float_pos_inf") return Value(std::numeric_limits<double>::infinity());
    if (name == "float_neg_inf") return Value(-std::numeric_limits<double>::infinity());
    if (name == "float_subnormal_min") return Value(double_from_bits(0x0000000000000001ULL));
    if (name == "text_unicode") return Value(std::string("h\xc3\xa9llo"));
    if (name == "text_x") return Value("x");
    if (name == "bytes_empty") {
        std::array<std::byte, 0> bytes = {};
        return Value::bytes(bytes);
    }
    if (name == "bytes_deadbeef") {
        std::array<std::byte, 4> bytes = {
            std::byte{0xde}, std::byte{0xad}, std::byte{0xbe}, std::byte{0xef},
        };
        return Value::bytes(bytes);
    }
    if (name == "bytes_256") {
        std::array<std::byte, 256> bytes = {};
        for (size_t i = 0; i < bytes.size(); ++i) {
            bytes[i] = std::byte{static_cast<unsigned char>(i)};
        }
        return Value::bytes(bytes);
    }
    if (name == "json_nested") {
        return Value::json(R"({"a":null,"z":[1,{"deep":[true,false]}]})");
    }
    if (name == "timestamp_zero") return Value::timestamp_seconds(0);
    if (name == "timestamp_max") return Value::timestamp_seconds(std::numeric_limits<int64_t>::max());
    if (name == "uuid_001122") return Value::uuid("00112233-4455-6677-8899-aabbccddeeff");
    if (name == "vector_empty") {
        std::array<float, 0> vector = {};
        return Value::vector(vector);
    }
    if (name == "vector_three") {
        std::array<float, 3> vector = {1.0f, 2.0f, -0.5f};
        return Value::vector(vector);
    }
    if (name == "vector_128") {
        std::array<float, 128> vector = {};
        for (size_t i = 0; i < vector.size(); ++i) {
            vector[i] = static_cast<float>(i);
        }
        return Value::vector(vector);
    }
    throw std::runtime_error("unknown fixture: " + name);
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
    std::array<Value, 3> params = {Value::int64(42), Value("x"), Value(std::nullopt)};
    auto encoded = encode_query_with_params("Q", params);
    EXPECT_EQ(encoded, std::vector<uint8_t>({
        1, 0, 0, 0, 'Q',
        3, 0, 0, 0,
        0x02, 42, 0, 0, 0, 0, 0, 0, 0,
        0x04, 1, 0, 0, 0, 'x',
        0x00,
    }));
}

TEST(ValueCodec, AcceptedCppSnippetParamShapeCompiles) {
    constexpr std::string_view sql = "SELECT $1";
    auto encoded = encode_query_with_params(sql, {Value::int64(42)});
    EXPECT_EQ(encoded[0], sql.size());
}

TEST(ValueCodec, HttpParamsUseJsonEnvelopesForTaggedValues) {
    std::array<uint8_t, 2> bytes = {static_cast<uint8_t>('h'), static_cast<uint8_t>('i')};
    std::array<float, 2> vec = {1.0f, 2.0f};
    std::array<Value, 10> params = {
        Value(std::nullopt),
        Value(true),
        Value::int64(42),
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

TEST(ValueCodec, SharedParameterFixturesMatchManifest) {
    const std::string manifest = read_fixture_manifest();
    const std::array<const char*, 22> names = {
        "null",
        "bool_true",
        "bool_false",
        "int_min",
        "int_max",
        "int_42",
        "float_nan",
        "float_pos_inf",
        "float_neg_inf",
        "float_subnormal_min",
        "text_unicode",
        "text_x",
        "bytes_empty",
        "bytes_deadbeef",
        "bytes_256",
        "json_nested",
        "timestamp_zero",
        "timestamp_max",
        "uuid_001122",
        "vector_empty",
        "vector_three",
        "vector_128",
    };

    for (const char* name : names) {
        EXPECT_EQ(encode_value(fixture_value(name)), hex_to_bytes(manifest_hex(manifest, name))) << name;
    }

    std::array<Value, 3> params = {fixture_value("int_42"), fixture_value("text_x"), fixture_value("null")};
    EXPECT_EQ(encode_query_with_params("SELECT $1", params),
              hex_to_bytes(manifest_hex(manifest, "select_one_mixed")));
}

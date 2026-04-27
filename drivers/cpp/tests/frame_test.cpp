// Frame encode/decode round-trips, plus the negative cases the
// codec is supposed to reject.

#include "reddb/errors.hpp"
#include "reddb/redwire/codec.hpp"
#include "reddb/redwire/frame.hpp"

#include <gtest/gtest.h>

#include <cstring>
#include <string>
#include <vector>

using namespace reddb::redwire;

TEST(Frame, RoundTripPlain) {
    Frame f;
    f.kind = MessageKind::Query;
    f.correlation_id = 7;
    f.stream_id = 0;
    const std::string sql = "SELECT 1";
    f.payload.assign(sql.begin(), sql.end());

    auto bytes = encode_frame(f);
    auto [decoded, n] = decode_frame(bytes.data(), bytes.size());
    EXPECT_EQ(n, bytes.size());
    EXPECT_EQ(decoded.kind, f.kind);
    EXPECT_EQ(decoded.correlation_id, f.correlation_id);
    EXPECT_EQ(decoded.payload, f.payload);
}

TEST(Frame, HeaderLayout) {
    Frame f;
    f.kind = MessageKind::Hello;
    f.correlation_id = 0x0102030405060708ULL;
    f.stream_id = 0xabcd;
    f.payload = {1, 2, 3, 4};
    auto bytes = encode_frame(f);

    ASSERT_EQ(bytes.size(), FRAME_HEADER_SIZE + 4);
    // length LE
    EXPECT_EQ(bytes[0], 20);
    EXPECT_EQ(bytes[1], 0);
    EXPECT_EQ(bytes[2], 0);
    EXPECT_EQ(bytes[3], 0);
    EXPECT_EQ(bytes[4], 0x10); // Hello
    EXPECT_EQ(bytes[5], 0x00); // no flags
    EXPECT_EQ(bytes[6], 0xcd); EXPECT_EQ(bytes[7], 0xab);
    // correlation id LE
    EXPECT_EQ(bytes[8],  0x08);
    EXPECT_EQ(bytes[9],  0x07);
    EXPECT_EQ(bytes[10], 0x06);
    EXPECT_EQ(bytes[11], 0x05);
    EXPECT_EQ(bytes[12], 0x04);
    EXPECT_EQ(bytes[13], 0x03);
    EXPECT_EQ(bytes[14], 0x02);
    EXPECT_EQ(bytes[15], 0x01);
}

TEST(Frame, RejectsTooLargeHeader) {
    // Hand-craft a header with an absurd length.
    std::vector<uint8_t> hdr(FRAME_HEADER_SIZE);
    uint32_t bad = MAX_FRAME_SIZE + 1;
    std::memcpy(hdr.data(), &bad, sizeof(bad));
    hdr[4] = 0x01;
    EXPECT_THROW(decode_frame(hdr.data(), hdr.size()), reddb::RedDBError);
}

TEST(Frame, RejectsTooSmallHeader) {
    std::vector<uint8_t> hdr(FRAME_HEADER_SIZE, 0);
    uint32_t bad = 8;
    std::memcpy(hdr.data(), &bad, sizeof(bad));
    hdr[4] = 0x01;
    EXPECT_THROW(decode_frame(hdr.data(), hdr.size()), reddb::RedDBError);
}

TEST(Frame, RejectsUnknownFlags) {
    Frame f;
    f.kind = MessageKind::Query;
    f.correlation_id = 1;
    f.payload = {0};
    auto bytes = encode_frame(f);
    bytes[5] = 0b1000'0000; // unknown bit
    EXPECT_THROW(decode_frame(bytes.data(), bytes.size()), reddb::RedDBError);
}

TEST(Frame, RejectsUnknownKind) {
    Frame f;
    f.kind = MessageKind::Query;
    f.correlation_id = 1;
    f.payload = {0};
    auto bytes = encode_frame(f);
    bytes[4] = 0x77; // not a known kind
    EXPECT_THROW(decode_frame(bytes.data(), bytes.size()), reddb::RedDBError);
}

TEST(Frame, BackToBackFrames) {
    // Stitch two frames into one buffer; decode them sequentially
    // and verify each comes back intact.
    Frame a;
    a.kind = MessageKind::Query;
    a.correlation_id = 1;
    a.payload = {'a', 'a'};
    Frame b;
    b.kind = MessageKind::Ping;
    b.correlation_id = 2;
    auto buf_a = encode_frame(a);
    auto buf_b = encode_frame(b);
    std::vector<uint8_t> all;
    all.insert(all.end(), buf_a.begin(), buf_a.end());
    all.insert(all.end(), buf_b.begin(), buf_b.end());

    auto [da, na] = decode_frame(all.data(), all.size());
    EXPECT_EQ(da.kind, MessageKind::Query);
    EXPECT_EQ(da.payload, std::vector<uint8_t>({'a','a'}));
    EXPECT_EQ(na, buf_a.size());

    auto [db, nb] = decode_frame(all.data() + na, all.size() - na);
    EXPECT_EQ(db.kind, MessageKind::Ping);
    EXPECT_EQ(nb, buf_b.size());
}

#if REDDB_HAS_ZSTD
TEST(Frame, ZstdCompressionRoundTrip) {
    std::vector<uint8_t> payload;
    payload.reserve(3000);
    for (int i = 0; i < 500; ++i) {
        payload.insert(payload.end(), {'a','b','c','a','b','c'});
    }
    Frame f;
    f.kind = MessageKind::Result;
    f.correlation_id = 1;
    f.flags.set(Flags::COMPRESSED);
    f.payload = payload;
    auto bytes = encode_frame(f);
    EXPECT_LT(bytes.size(), payload.size() + FRAME_HEADER_SIZE);

    auto [decoded, _n] = decode_frame(bytes.data(), bytes.size());
    EXPECT_EQ(decoded.payload, payload);
    EXPECT_TRUE(decoded.flags.contains(Flags::COMPRESSED));
}
#else
TEST(Frame, CompressedThrowsWithoutZstd) {
    Frame f;
    f.kind = MessageKind::Result;
    f.correlation_id = 1;
    f.flags.set(Flags::COMPRESSED);
    f.payload = {1, 2, 3};
    EXPECT_THROW(encode_frame(f), reddb::RedDBError);
}
#endif

TEST(Frame, FlagsCombine) {
    Flags f;
    f.set(Flags::COMPRESSED);
    f.set(Flags::MORE_FRAMES);
    EXPECT_TRUE(f.contains(Flags::COMPRESSED));
    EXPECT_TRUE(f.contains(Flags::MORE_FRAMES));
    EXPECT_EQ(f.bits, 0b11);
}

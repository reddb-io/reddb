// RedWire frame layout. Mirrors `src/wire/redwire/frame.rs`.
//
// Header (16 bytes, little-endian):
//   u32 length         total frame size, incl. header
//   u8  kind           MessageKind discriminator
//   u8  flags          COMPRESSED | MORE_FRAMES
//   u16 stream_id      0 = unsolicited
//   u64 correlation_id request↔response pairing

#pragma once

#include <cstdint>
#include <vector>

namespace reddb::redwire {

constexpr size_t FRAME_HEADER_SIZE = 16;
constexpr uint32_t MAX_FRAME_SIZE = 16u * 1024u * 1024u; // 16 MiB
constexpr uint8_t KNOWN_FLAGS = 0b0000'0011;

enum class MessageKind : uint8_t {
    Query = 0x01,
    Result = 0x02,
    Error = 0x03,
    BulkInsert = 0x04,
    BulkOk = 0x05,
    BulkInsertBinary = 0x06,
    QueryBinary = 0x07,
    BulkInsertPrevalidated = 0x08,

    Hello = 0x10,
    HelloAck = 0x11,
    AuthRequest = 0x12,
    AuthResponse = 0x13,
    AuthOk = 0x14,
    AuthFail = 0x15,
    Bye = 0x16,
    Ping = 0x17,
    Pong = 0x18,
    Get = 0x19,
    Delete = 0x1A,
    DeleteOk = 0x1B,
};

bool is_known_kind(uint8_t byte) noexcept;

struct Flags {
    static constexpr uint8_t COMPRESSED = 0b0000'0001;
    static constexpr uint8_t MORE_FRAMES = 0b0000'0010;

    uint8_t bits = 0;

    constexpr Flags() = default;
    constexpr explicit Flags(uint8_t b) : bits(b) {}

    constexpr bool contains(uint8_t f) const { return (bits & f) == f; }
    constexpr Flags& set(uint8_t f) { bits |= f; return *this; }
};

struct FrameHeader {
    uint32_t length = 0;
    MessageKind kind = MessageKind::Query;
    Flags flags{};
    uint16_t stream_id = 0;
    uint64_t correlation_id = 0;
};

struct Frame {
    MessageKind kind = MessageKind::Query;
    Flags flags{};
    uint16_t stream_id = 0;
    uint64_t correlation_id = 0;
    std::vector<uint8_t> payload;
};

// Header (de)serialization helpers — used by tests + by streaming
// readers that need length up-front.
void encode_header(const FrameHeader& h, uint8_t out[FRAME_HEADER_SIZE]);
FrameHeader decode_header(const uint8_t in[FRAME_HEADER_SIZE]);

} // namespace reddb::redwire

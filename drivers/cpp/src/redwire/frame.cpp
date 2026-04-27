#include "reddb/redwire/frame.hpp"

#include <cstring>

namespace reddb::redwire {

bool is_known_kind(uint8_t byte) noexcept {
    switch (byte) {
        case 0x01: case 0x02: case 0x03: case 0x04:
        case 0x05: case 0x06: case 0x07: case 0x08:
        case 0x10: case 0x11: case 0x12: case 0x13:
        case 0x14: case 0x15: case 0x16: case 0x17:
        case 0x18: case 0x19: case 0x1A: case 0x1B:
            return true;
        default:
            return false;
    }
}

static void put_u16_le(uint8_t* p, uint16_t v) {
    p[0] = static_cast<uint8_t>(v & 0xff);
    p[1] = static_cast<uint8_t>((v >> 8) & 0xff);
}
static void put_u32_le(uint8_t* p, uint32_t v) {
    p[0] = static_cast<uint8_t>(v & 0xff);
    p[1] = static_cast<uint8_t>((v >> 8) & 0xff);
    p[2] = static_cast<uint8_t>((v >> 16) & 0xff);
    p[3] = static_cast<uint8_t>((v >> 24) & 0xff);
}
static void put_u64_le(uint8_t* p, uint64_t v) {
    for (int i = 0; i < 8; ++i) {
        p[i] = static_cast<uint8_t>((v >> (i * 8)) & 0xff);
    }
}
static uint16_t get_u16_le(const uint8_t* p) {
    return static_cast<uint16_t>(p[0]) | (static_cast<uint16_t>(p[1]) << 8);
}
static uint32_t get_u32_le(const uint8_t* p) {
    return static_cast<uint32_t>(p[0]) |
           (static_cast<uint32_t>(p[1]) << 8) |
           (static_cast<uint32_t>(p[2]) << 16) |
           (static_cast<uint32_t>(p[3]) << 24);
}
static uint64_t get_u64_le(const uint8_t* p) {
    uint64_t v = 0;
    for (int i = 0; i < 8; ++i) {
        v |= static_cast<uint64_t>(p[i]) << (i * 8);
    }
    return v;
}

void encode_header(const FrameHeader& h, uint8_t out[FRAME_HEADER_SIZE]) {
    put_u32_le(out + 0, h.length);
    out[4] = static_cast<uint8_t>(h.kind);
    out[5] = h.flags.bits;
    put_u16_le(out + 6, h.stream_id);
    put_u64_le(out + 8, h.correlation_id);
}

FrameHeader decode_header(const uint8_t in[FRAME_HEADER_SIZE]) {
    FrameHeader h;
    h.length = get_u32_le(in + 0);
    h.kind = static_cast<MessageKind>(in[4]);
    h.flags = Flags{in[5]};
    h.stream_id = get_u16_le(in + 6);
    h.correlation_id = get_u64_le(in + 8);
    return h;
}

} // namespace reddb::redwire

#include "reddb/redwire/codec.hpp"
#include "reddb/errors.hpp"

#include <cstring>

#if REDDB_HAS_ZSTD
#include <zstd.h>
#endif

namespace reddb::redwire {

bool zstd_available() noexcept {
#if REDDB_HAS_ZSTD
    return true;
#else
    return false;
#endif
}

namespace {

#if REDDB_HAS_ZSTD
std::vector<uint8_t> zstd_compress(const std::vector<uint8_t>& src, int level) {
    size_t bound = ZSTD_compressBound(src.size());
    std::vector<uint8_t> out(bound);
    size_t n = ZSTD_compress(out.data(), bound, src.data(), src.size(), level);
    if (ZSTD_isError(n)) {
        throw RedDBError(ErrorCode::Protocol,
                         std::string("zstd compress: ") + ZSTD_getErrorName(n));
    }
    out.resize(n);
    return out;
}

std::vector<uint8_t> zstd_decompress(const uint8_t* src, size_t n) {
    // Try the framed size first; fall back to streaming for unknown sizes.
    unsigned long long size = ZSTD_getFrameContentSize(src, n);
    if (size != ZSTD_CONTENTSIZE_ERROR && size != ZSTD_CONTENTSIZE_UNKNOWN) {
        std::vector<uint8_t> out(static_cast<size_t>(size));
        size_t got = ZSTD_decompress(out.data(), out.size(), src, n);
        if (ZSTD_isError(got)) {
            throw RedDBError(ErrorCode::Protocol,
                             std::string("zstd decompress: ") + ZSTD_getErrorName(got));
        }
        out.resize(got);
        return out;
    }
    // Streaming decode.
    std::vector<uint8_t> out;
    ZSTD_DStream* ds = ZSTD_createDStream();
    if (!ds) {
        throw RedDBError(ErrorCode::Protocol, "zstd: createDStream failed");
    }
    ZSTD_initDStream(ds);
    ZSTD_inBuffer in{src, n, 0};
    uint8_t buf[8192];
    while (in.pos < in.size) {
        ZSTD_outBuffer ob{buf, sizeof(buf), 0};
        size_t r = ZSTD_decompressStream(ds, &ob, &in);
        if (ZSTD_isError(r)) {
            ZSTD_freeDStream(ds);
            throw RedDBError(ErrorCode::Protocol,
                             std::string("zstd decompress: ") + ZSTD_getErrorName(r));
        }
        out.insert(out.end(), buf, buf + ob.pos);
        if (r == 0) break;
    }
    ZSTD_freeDStream(ds);
    return out;
}
#endif

} // namespace

std::vector<uint8_t> encode_frame(const Frame& frame, int zstd_level) {
    std::vector<uint8_t> body = frame.payload;
    Flags flags = frame.flags;

    if (flags.contains(Flags::COMPRESSED)) {
#if REDDB_HAS_ZSTD
        body = zstd_compress(frame.payload, zstd_level);
#else
        (void)zstd_level;
        throw RedDBError(ErrorCode::CompressedButNoZstd,
                         "frame requested COMPRESSED but driver was built without zstd");
#endif
    }

    uint32_t total = static_cast<uint32_t>(FRAME_HEADER_SIZE + body.size());
    if (total > MAX_FRAME_SIZE) {
        throw RedDBError(ErrorCode::Protocol,
                         "encoded frame exceeds MAX_FRAME_SIZE (16 MiB)");
    }

    FrameHeader h;
    h.length = total;
    h.kind = frame.kind;
    h.flags = flags;
    h.stream_id = frame.stream_id;
    h.correlation_id = frame.correlation_id;

    std::vector<uint8_t> out(total);
    encode_header(h, out.data());
    if (!body.empty()) {
        std::memcpy(out.data() + FRAME_HEADER_SIZE, body.data(), body.size());
    }
    return out;
}

std::pair<Frame, size_t> decode_frame(const uint8_t* bytes, size_t len) {
    if (len < FRAME_HEADER_SIZE) {
        throw RedDBError(ErrorCode::Protocol, "frame header truncated");
    }
    FrameHeader h = decode_header(bytes);
    if (h.length < FRAME_HEADER_SIZE || h.length > MAX_FRAME_SIZE) {
        throw RedDBError(ErrorCode::Protocol,
                         "frame length invalid: " + std::to_string(h.length));
    }
    if (len < h.length) {
        throw RedDBError(ErrorCode::Protocol, "frame payload truncated");
    }
    if (!is_known_kind(static_cast<uint8_t>(h.kind))) {
        throw RedDBError(ErrorCode::Protocol,
                         "unknown message kind 0x" +
                         [&]() {
                             char b[3];
                             snprintf(b, sizeof(b), "%02x", static_cast<uint8_t>(h.kind));
                             return std::string(b);
                         }());
    }
    if ((h.flags.bits & ~KNOWN_FLAGS) != 0) {
        throw RedDBError(ErrorCode::Protocol,
                         "unknown flag bits 0x" +
                         [&]() {
                             char b[3];
                             snprintf(b, sizeof(b), "%02x", h.flags.bits);
                             return std::string(b);
                         }());
    }

    size_t payload_len = h.length - FRAME_HEADER_SIZE;
    const uint8_t* on_wire = bytes + FRAME_HEADER_SIZE;

    Frame frame;
    frame.kind = h.kind;
    frame.flags = h.flags;
    frame.stream_id = h.stream_id;
    frame.correlation_id = h.correlation_id;

    if (h.flags.contains(Flags::COMPRESSED)) {
#if REDDB_HAS_ZSTD
        frame.payload = zstd_decompress(on_wire, payload_len);
#else
        throw RedDBError(ErrorCode::CompressedButNoZstd,
                         "frame is COMPRESSED but driver was built without zstd");
#endif
    } else {
        frame.payload.assign(on_wire, on_wire + payload_len);
    }
    return {std::move(frame), static_cast<size_t>(h.length)};
}

} // namespace reddb::redwire

// Frame encode / decode + zstd glue. zstd is optional at build
// time: when missing, encoding never sets the COMPRESSED flag and
// decoding throws `CompressedButNoZstd` if the server sets it.

#pragma once

#include "reddb/redwire/frame.hpp"

#include <cstdint>
#include <utility>
#include <vector>

namespace reddb::redwire {

// Encode a frame including the 16-byte header and (optionally
// compressed) payload. If `frame.flags.COMPRESSED` is set and
// zstd is unavailable, throws `CompressedButNoZstd`.
std::vector<uint8_t> encode_frame(const Frame& frame, int zstd_level = 1);

// Decode a frame from `bytes`. Returns the parsed frame and the
// number of bytes consumed (== frame.length on the wire).
// Throws `RedDBError(Protocol)` for malformed frames; throws
// `RedDBError(CompressedButNoZstd)` if the COMPRESSED bit is set
// and zstd is unavailable.
std::pair<Frame, size_t> decode_frame(const uint8_t* bytes, size_t len);

// Returns true if zstd was linked at build time.
bool zstd_available() noexcept;

} // namespace reddb::redwire

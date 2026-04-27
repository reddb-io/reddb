// SCRAM-SHA-256 client primitives. Mirrors `src/auth/scram.rs` and
// the Rust driver's `drivers/rust/src/redwire/scram.rs` byte-for-byte.
//
// Wire shape (RFC 5802):
//   client-first  = "n,," + "n=" + user + ",r=" + cnonce
//   server-first  = "r=" + combined_nonce + ",s=" + b64(salt) + ",i=" + iter
//   client-final  = "c=biws,r=" + combined_nonce + ",p=" + b64(client_proof)
//   server-final  = "v=" + b64(server_signature)
//
// Where:
//   salted_password = PBKDF2-HMAC-SHA256(password, salt, iter, 32)
//   client_key      = HMAC-SHA256(salted_password, "Client Key")
//   stored_key      = SHA256(client_key)
//   client_signature= HMAC-SHA256(stored_key, AuthMessage)
//   client_proof    = client_key XOR client_signature
//   server_key      = HMAC-SHA256(salted_password, "Server Key")
//   server_signature= HMAC-SHA256(server_key, AuthMessage)
//
// AuthMessage = client-first-bare + "," + server-first + "," + client-final-without-proof

const std = @import("std");
const Allocator = std.mem.Allocator;

const HmacSha256 = std.crypto.auth.hmac.sha2.HmacSha256;
const Sha256 = std.crypto.hash.sha2.Sha256;
const pbkdf2 = std.crypto.pwhash.pbkdf2;

pub const HASH_LEN: usize = 32;

pub fn hmacSha256(key: []const u8, data: []const u8) [HASH_LEN]u8 {
    var out: [HASH_LEN]u8 = undefined;
    HmacSha256.create(&out, data, key);
    return out;
}

pub fn sha256(data: []const u8) [HASH_LEN]u8 {
    var out: [HASH_LEN]u8 = undefined;
    Sha256.hash(data, &out, .{});
    return out;
}

/// PBKDF2-HMAC-SHA256 with a fixed 32-byte derived-key length.
pub fn pbkdf2Sha256(password: []const u8, salt: []const u8, iter: u32) ![HASH_LEN]u8 {
    var out: [HASH_LEN]u8 = undefined;
    try pbkdf2(&out, password, salt, iter, HmacSha256);
    return out;
}

pub fn xor(a: []const u8, b: []const u8, dst: []u8) void {
    std.debug.assert(a.len == b.len and dst.len == a.len);
    for (a, b, dst) |x, y, *o| o.* = x ^ y;
}

/// Compute the client proof. Used by the driver to send the
/// `client-final` message — same shape as the engine's
/// `auth/scram.rs::client_proof`.
pub fn clientProof(
    password: []const u8,
    salt: []const u8,
    iter: u32,
    auth_message: []const u8,
) ![HASH_LEN]u8 {
    const salted = try pbkdf2Sha256(password, salt, iter);
    const client_key = hmacSha256(&salted, "Client Key");
    const stored_key = sha256(&client_key);
    const signature = hmacSha256(&stored_key, auth_message);
    var out: [HASH_LEN]u8 = undefined;
    xor(&client_key, &signature, &out);
    return out;
}

pub fn verifyServerSignature(
    password: []const u8,
    salt: []const u8,
    iter: u32,
    auth_message: []const u8,
    presented: []const u8,
) !bool {
    if (presented.len != HASH_LEN) return false;
    const salted = try pbkdf2Sha256(password, salt, iter);
    const server_key = hmacSha256(&salted, "Server Key");
    const expected = hmacSha256(&server_key, auth_message);
    return std.crypto.utils.timingSafeEql([HASH_LEN]u8, expected, presented[0..HASH_LEN].*);
}

// ---------------------------------------------------------------------------
// Helpers used by the handshake state machine. Tiny wrappers around
// std.base64.standard so the call sites stay readable.
// ---------------------------------------------------------------------------

pub const Encoder = std.base64.standard.Encoder;
pub const Decoder = std.base64.standard.Decoder;

pub fn b64Encode(allocator: Allocator, src: []const u8) ![]u8 {
    const out = try allocator.alloc(u8, Encoder.calcSize(src.len));
    errdefer allocator.free(out);
    _ = Encoder.encode(out, src);
    return out;
}

pub fn b64DecodeAlloc(allocator: Allocator, src: []const u8) ![]u8 {
    const max = try Decoder.calcSizeForSlice(src);
    const out = try allocator.alloc(u8, max);
    errdefer allocator.free(out);
    try Decoder.decode(out, src);
    return out;
}

/// Generate a fresh client nonce. RFC 5802 doesn't pin the format —
/// we follow PostgreSQL's convention: 24 random bytes, base64'd
/// without padding-trimming, matching what `examples/stress_wire_client`
/// already produces.
pub fn generateClientNonce(allocator: Allocator) ![]u8 {
    var bytes: [24]u8 = undefined;
    std.crypto.random.bytes(&bytes);
    return b64Encode(allocator, &bytes);
}

/// Parse `r=...,s=...,i=...` out of a server-first message. Returns
/// borrowed slices of the input; combined nonce is returned for the
/// client to echo back in `client-final`.
pub const ServerFirst = struct {
    combined_nonce: []const u8,
    salt_b64: []const u8,
    iter: u32,
};

pub fn parseServerFirst(server_first: []const u8) !ServerFirst {
    var combined_nonce: ?[]const u8 = null;
    var salt_b64: ?[]const u8 = null;
    var iter: ?u32 = null;
    var it = std.mem.splitScalar(u8, server_first, ',');
    while (it.next()) |part| {
        if (part.len < 2) continue;
        if (std.mem.startsWith(u8, part, "r=")) {
            combined_nonce = part[2..];
        } else if (std.mem.startsWith(u8, part, "s=")) {
            salt_b64 = part[2..];
        } else if (std.mem.startsWith(u8, part, "i=")) {
            iter = std.fmt.parseInt(u32, part[2..], 10) catch null;
        }
    }
    if (combined_nonce == null or salt_b64 == null or iter == null) {
        return error.ScramBadServerFirst;
    }
    return ServerFirst{
        .combined_nonce = combined_nonce.?,
        .salt_b64 = salt_b64.?,
        .iter = iter.?,
    };
}

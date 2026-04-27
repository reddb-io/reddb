#include "reddb/redwire/scram.hpp"
#include "reddb/errors.hpp"

#include <openssl/evp.h>
#include <openssl/hmac.h>
#include <openssl/rand.h>

#include <cstring>
#include <stdexcept>

namespace reddb::redwire::scram {

std::array<uint8_t, 32> hmac_sha256(const uint8_t* key, size_t key_len,
                                    const uint8_t* data, size_t data_len) {
    std::array<uint8_t, 32> out{};
    unsigned int out_len = 0;
    if (HMAC(EVP_sha256(), key, static_cast<int>(key_len),
             data, data_len, out.data(), &out_len) == nullptr ||
        out_len != 32) {
        throw RedDBError(ErrorCode::Tls, "HMAC-SHA256 failed");
    }
    return out;
}

std::array<uint8_t, 32> sha256(const uint8_t* data, size_t len) {
    std::array<uint8_t, 32> out{};
    EVP_MD_CTX* ctx = EVP_MD_CTX_new();
    if (!ctx) throw RedDBError(ErrorCode::Tls, "EVP_MD_CTX_new failed");
    int ok = EVP_DigestInit_ex(ctx, EVP_sha256(), nullptr) &&
             EVP_DigestUpdate(ctx, data, len);
    unsigned int out_len = 0;
    if (ok) ok = EVP_DigestFinal_ex(ctx, out.data(), &out_len);
    EVP_MD_CTX_free(ctx);
    if (!ok || out_len != 32) {
        throw RedDBError(ErrorCode::Tls, "SHA-256 failed");
    }
    return out;
}

std::array<uint8_t, 32> pbkdf2_sha256(const uint8_t* password, size_t password_len,
                                       const uint8_t* salt, size_t salt_len,
                                       uint32_t iter) {
    std::array<uint8_t, 32> out{};
    if (PKCS5_PBKDF2_HMAC(reinterpret_cast<const char*>(password),
                          static_cast<int>(password_len),
                          salt, static_cast<int>(salt_len),
                          static_cast<int>(iter),
                          EVP_sha256(),
                          static_cast<int>(out.size()),
                          out.data()) != 1) {
        throw RedDBError(ErrorCode::Tls, "PBKDF2-HMAC-SHA256 failed");
    }
    return out;
}

std::vector<uint8_t> xor_bytes(const uint8_t* a, const uint8_t* b, size_t n) {
    std::vector<uint8_t> out(n);
    for (size_t i = 0; i < n; ++i) out[i] = a[i] ^ b[i];
    return out;
}

std::vector<uint8_t> client_proof(const std::string& password,
                                  const std::vector<uint8_t>& salt,
                                  uint32_t iter,
                                  const std::vector<uint8_t>& auth_message) {
    auto salted = pbkdf2_sha256(reinterpret_cast<const uint8_t*>(password.data()),
                                password.size(), salt.data(), salt.size(), iter);
    auto client_key = hmac_sha256(salted.data(), salted.size(),
                                  reinterpret_cast<const uint8_t*>("Client Key"), 10);
    auto stored_key = sha256(client_key.data(), client_key.size());
    auto signature = hmac_sha256(stored_key.data(), stored_key.size(),
                                 auth_message.data(), auth_message.size());
    return xor_bytes(client_key.data(), signature.data(), 32);
}

bool verify_server_signature(const std::string& password,
                             const std::vector<uint8_t>& salt,
                             uint32_t iter,
                             const std::vector<uint8_t>& auth_message,
                             const std::vector<uint8_t>& presented_signature) {
    if (presented_signature.size() != 32) return false;
    auto salted = pbkdf2_sha256(reinterpret_cast<const uint8_t*>(password.data()),
                                password.size(), salt.data(), salt.size(), iter);
    auto server_key = hmac_sha256(salted.data(), salted.size(),
                                  reinterpret_cast<const uint8_t*>("Server Key"), 10);
    auto expected = hmac_sha256(server_key.data(), server_key.size(),
                                auth_message.data(), auth_message.size());
    return constant_time_eq(expected.data(), presented_signature.data(), 32);
}

bool constant_time_eq(const uint8_t* a, const uint8_t* b, size_t n) {
    uint8_t diff = 0;
    for (size_t i = 0; i < n; ++i) diff |= a[i] ^ b[i];
    return diff == 0;
}

std::string make_client_nonce() {
    uint8_t buf[24];
    if (RAND_bytes(buf, sizeof(buf)) != 1) {
        throw RedDBError(ErrorCode::Tls, "RAND_bytes failed");
    }
    return base64_encode(buf, sizeof(buf));
}

// Hand-rolled base64 (RFC 4648 standard alphabet, padded).

static constexpr char B64_ALPHA[65] =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

std::string base64_encode(const uint8_t* data, size_t len) {
    std::string out;
    out.reserve((len + 2) / 3 * 4);
    size_t i = 0;
    for (; i + 3 <= len; i += 3) {
        uint32_t n = (uint32_t(data[i]) << 16) | (uint32_t(data[i + 1]) << 8) | data[i + 2];
        out.push_back(B64_ALPHA[(n >> 18) & 0x3F]);
        out.push_back(B64_ALPHA[(n >> 12) & 0x3F]);
        out.push_back(B64_ALPHA[(n >> 6) & 0x3F]);
        out.push_back(B64_ALPHA[n & 0x3F]);
    }
    size_t rem = len - i;
    if (rem == 1) {
        uint32_t n = uint32_t(data[i]) << 16;
        out.push_back(B64_ALPHA[(n >> 18) & 0x3F]);
        out.push_back(B64_ALPHA[(n >> 12) & 0x3F]);
        out.push_back('=');
        out.push_back('=');
    } else if (rem == 2) {
        uint32_t n = (uint32_t(data[i]) << 16) | (uint32_t(data[i + 1]) << 8);
        out.push_back(B64_ALPHA[(n >> 18) & 0x3F]);
        out.push_back(B64_ALPHA[(n >> 12) & 0x3F]);
        out.push_back(B64_ALPHA[(n >> 6) & 0x3F]);
        out.push_back('=');
    }
    return out;
}

std::vector<uint8_t> base64_decode(const std::string& s) {
    std::vector<uint8_t> out;
    out.reserve(s.size() * 3 / 4);
    uint32_t buf = 0;
    int bits = 0;
    for (char c : s) {
        if (c == '=') break;
        int v;
        if (c >= 'A' && c <= 'Z') v = c - 'A';
        else if (c >= 'a' && c <= 'z') v = c - 'a' + 26;
        else if (c >= '0' && c <= '9') v = c - '0' + 52;
        else if (c == '+') v = 62;
        else if (c == '/') v = 63;
        else throw RedDBError(ErrorCode::Protocol, "invalid base64 character");
        buf = (buf << 6) | uint32_t(v);
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            out.push_back(uint8_t((buf >> bits) & 0xFF));
        }
    }
    return out;
}

static char hex_nibble(uint8_t n) { return n < 10 ? char('0' + n) : char('a' + n - 10); }

std::string hex_encode(const uint8_t* data, size_t len) {
    std::string out;
    out.reserve(len * 2);
    for (size_t i = 0; i < len; ++i) {
        out.push_back(hex_nibble((data[i] >> 4) & 0xF));
        out.push_back(hex_nibble(data[i] & 0xF));
    }
    return out;
}

std::vector<uint8_t> hex_decode(const std::string& s) {
    if (s.size() % 2 != 0) throw RedDBError(ErrorCode::Protocol, "hex string has odd length");
    auto h = [](char c) -> int {
        if (c >= '0' && c <= '9') return c - '0';
        if (c >= 'a' && c <= 'f') return c - 'a' + 10;
        if (c >= 'A' && c <= 'F') return c - 'A' + 10;
        return -1;
    };
    std::vector<uint8_t> out;
    out.reserve(s.size() / 2);
    for (size_t i = 0; i < s.size(); i += 2) {
        int hi = h(s[i]), lo = h(s[i + 1]);
        if (hi < 0 || lo < 0) throw RedDBError(ErrorCode::Protocol, "invalid hex character");
        out.push_back(static_cast<uint8_t>((hi << 4) | lo));
    }
    return out;
}

} // namespace reddb::redwire::scram

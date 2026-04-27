// SCRAM-SHA-256 (RFC 5802) client primitives, backed by OpenSSL EVP.
// Mirrors the engine's `src/auth/scram.rs`.

#pragma once

#include <array>
#include <cstdint>
#include <string>
#include <vector>

namespace reddb::redwire::scram {

constexpr uint32_t DEFAULT_ITER = 16384;
constexpr uint32_t MIN_ITER = 4096;

// HMAC-SHA256(key, data) → 32 bytes.
std::array<uint8_t, 32> hmac_sha256(const uint8_t* key, size_t key_len,
                                    const uint8_t* data, size_t data_len);

// SHA-256(data) → 32 bytes.
std::array<uint8_t, 32> sha256(const uint8_t* data, size_t len);

// PBKDF2-HMAC-SHA256, 32-byte derived key.
std::array<uint8_t, 32> pbkdf2_sha256(const uint8_t* password, size_t password_len,
                                       const uint8_t* salt, size_t salt_len,
                                       uint32_t iter);

std::vector<uint8_t> xor_bytes(const uint8_t* a, const uint8_t* b, size_t n);

// ClientProof = ClientKey XOR HMAC(StoredKey, AuthMessage).
// Returns 32 bytes.
std::vector<uint8_t> client_proof(const std::string& password,
                                  const std::vector<uint8_t>& salt,
                                  uint32_t iter,
                                  const std::vector<uint8_t>& auth_message);

// Verify the server's signature constant-time.
bool verify_server_signature(const std::string& password,
                             const std::vector<uint8_t>& salt,
                             uint32_t iter,
                             const std::vector<uint8_t>& auth_message,
                             const std::vector<uint8_t>& presented_signature);

// 24-byte client nonce, base64 (standard alphabet).
std::string make_client_nonce();

// Standard base64 helpers (RFC 4648, padded).
std::string base64_encode(const uint8_t* data, size_t len);
std::vector<uint8_t> base64_decode(const std::string& s);

// Hex helpers — server signatures are hex-encoded in some tests.
std::string hex_encode(const uint8_t* data, size_t len);
std::vector<uint8_t> hex_decode(const std::string& s);

bool constant_time_eq(const uint8_t* a, const uint8_t* b, size_t n);

} // namespace reddb::redwire::scram

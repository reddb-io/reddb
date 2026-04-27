// SCRAM-SHA-256 primitives + a recorded round-trip exchange.

#include "reddb/redwire/scram.hpp"

#include <gtest/gtest.h>

#include <array>
#include <cstring>
#include <string>
#include <vector>

using namespace reddb::redwire;

// RFC 4231 test case 1 — HMAC-SHA-256 known answer.
TEST(Scram, HmacSha256Rfc4231Case1) {
    std::array<uint8_t, 20> key;
    std::memset(key.data(), 0x0b, key.size());
    const char data[] = "Hi There";
    auto mac = scram::hmac_sha256(key.data(), key.size(),
                                  reinterpret_cast<const uint8_t*>(data), 8);
    const uint8_t expected[32] = {
        0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53,
        0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b, 0xf1, 0x2b,
        0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7,
        0x26, 0xe9, 0x37, 0x6c, 0x2e, 0x32, 0xcf, 0xf7,
    };
    for (size_t i = 0; i < 32; ++i) {
        EXPECT_EQ(mac[i], expected[i]) << "byte " << i;
    }
}

TEST(Scram, Pbkdf2Deterministic) {
    auto a = scram::pbkdf2_sha256(reinterpret_cast<const uint8_t*>("password"), 8,
                                  reinterpret_cast<const uint8_t*>("salt"), 4, 4096);
    auto b = scram::pbkdf2_sha256(reinterpret_cast<const uint8_t*>("password"), 8,
                                  reinterpret_cast<const uint8_t*>("salt"), 4, 4096);
    EXPECT_EQ(a, b);
    auto c = scram::pbkdf2_sha256(reinterpret_cast<const uint8_t*>("different"), 9,
                                  reinterpret_cast<const uint8_t*>("salt"), 4, 4096);
    EXPECT_NE(a, c);
}

TEST(Scram, ProofRoundTripDeterministic) {
    std::vector<uint8_t> salt = {'r','e','d','d','b','-','t','e','s','t'};
    std::vector<uint8_t> auth(reinterpret_cast<const uint8_t*>("client-first-bare,server-first,client-final-no-proof"),
                              reinterpret_cast<const uint8_t*>("client-first-bare,server-first,client-final-no-proof") + 52);
    auto p1 = scram::client_proof("hunter2", salt, 4096, auth);
    auto p2 = scram::client_proof("hunter2", salt, 4096, auth);
    EXPECT_EQ(p1, p2);
    auto p3 = scram::client_proof("wrong", salt, 4096, auth);
    EXPECT_NE(p1, p3);
}

// Full SCRAM round trip — derive the verifier on the client side
// (the same way the engine derives it on creation), simulate the
// server signing the auth message, and verify the round trip.
TEST(Scram, FullExchangeAgainstRecordedServerFirst) {
    const std::string password = "correct horse";
    std::vector<uint8_t> salt = {'r','e','d','d','b','-','r','t','-','s','a','l','t'};
    const uint32_t iter = 4096;

    // Match what `src/auth/scram.rs::full_round_trip` builds.
    const std::string client_first_bare = "n=alice,r=cnonce";
    const std::string server_first = "r=cnonce+snonce,s=cmVkZGItcnQtc2FsdA==,i=4096";
    const std::string client_final_no_proof = "c=biws,r=cnonce+snonce";

    std::string am_str = client_first_bare + "," + server_first + "," + client_final_no_proof;
    std::vector<uint8_t> am(am_str.begin(), am_str.end());

    auto proof = scram::client_proof(password, salt, iter, am);

    // Server-side verification: ClientKey = proof XOR HMAC(stored_key, am)
    auto salted = scram::pbkdf2_sha256(
        reinterpret_cast<const uint8_t*>(password.data()), password.size(),
        salt.data(), salt.size(), iter);
    auto client_key = scram::hmac_sha256(salted.data(), salted.size(),
                                         reinterpret_cast<const uint8_t*>("Client Key"), 10);
    auto stored_key = scram::sha256(client_key.data(), client_key.size());
    auto signature = scram::hmac_sha256(stored_key.data(), stored_key.size(),
                                        am.data(), am.size());
    auto recovered_key = scram::xor_bytes(proof.data(), signature.data(), 32);
    auto derived_stored = scram::sha256(recovered_key.data(), recovered_key.size());
    EXPECT_TRUE(scram::constant_time_eq(derived_stored.data(), stored_key.data(), 32));

    // Server signature round trip — verifier reproduces the signature
    // the server would have sent in AuthOk.
    auto server_key = scram::hmac_sha256(salted.data(), salted.size(),
                                         reinterpret_cast<const uint8_t*>("Server Key"), 10);
    auto sig = scram::hmac_sha256(server_key.data(), server_key.size(),
                                  am.data(), am.size());
    std::vector<uint8_t> sig_v(sig.begin(), sig.end());
    EXPECT_TRUE(scram::verify_server_signature(password, salt, iter, am, sig_v));

    // Tampered signature is rejected.
    sig_v[0] ^= 0xFF;
    EXPECT_FALSE(scram::verify_server_signature(password, salt, iter, am, sig_v));
}

TEST(Scram, Base64RoundTrip) {
    const uint8_t data[] = {0xfe, 0x01, 0xab, 0xcd, 0xef, 0x10, 0x20, 0x30};
    auto b64 = scram::base64_encode(data, sizeof(data));
    auto back = scram::base64_decode(b64);
    ASSERT_EQ(back.size(), sizeof(data));
    for (size_t i = 0; i < sizeof(data); ++i) {
        EXPECT_EQ(back[i], data[i]);
    }
}

TEST(Scram, HexRoundTrip) {
    const uint8_t data[] = {0x00, 0xff, 0xa5, 0x5a};
    auto hex = scram::hex_encode(data, sizeof(data));
    EXPECT_EQ(hex, "00ffa55a");
    auto back = scram::hex_decode(hex);
    ASSERT_EQ(back.size(), sizeof(data));
    for (size_t i = 0; i < sizeof(data); ++i) {
        EXPECT_EQ(back[i], data[i]);
    }
}

TEST(Scram, ClientNonceIs24BytesBase64) {
    auto n1 = scram::make_client_nonce();
    auto n2 = scram::make_client_nonce();
    EXPECT_EQ(n1.size(), 32u); // 24 bytes → 32 chars base64 (no padding because 24 % 3 == 0)
    EXPECT_NE(n1, n2);         // overwhelmingly likely
    auto decoded = scram::base64_decode(n1);
    EXPECT_EQ(decoded.size(), 24u);
}

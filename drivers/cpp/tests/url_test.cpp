// URL parser tests. Targets ≥20 cases across schemes, ports, auth,
// query strings and embedded rejection.

#include "reddb/errors.hpp"
#include "reddb/url.hpp"

#include <gtest/gtest.h>

using namespace reddb;

TEST(Url, RedDefaultPort) {
    auto u = parse_uri("red://localhost");
    EXPECT_EQ(u.kind, UrlKind::Red);
    EXPECT_EQ(u.host, "localhost");
    EXPECT_EQ(u.port, 5050);
}

TEST(Url, RedExplicitPort) {
    auto u = parse_uri("red://example.com:6000");
    EXPECT_EQ(u.kind, UrlKind::Red);
    EXPECT_EQ(u.port, 6000);
}

TEST(Url, RedsExplicitTls) {
    auto u = parse_uri("reds://primary:5051");
    EXPECT_EQ(u.kind, UrlKind::Reds);
    EXPECT_EQ(u.port, 5051);
}

TEST(Url, RedsDefaultPort) {
    auto u = parse_uri("reds://primary");
    EXPECT_EQ(u.kind, UrlKind::Reds);
    EXPECT_EQ(u.port, 5051);
}

TEST(Url, HttpDefaultPort) {
    auto u = parse_uri("http://api.example.com");
    EXPECT_EQ(u.kind, UrlKind::Http);
    EXPECT_EQ(u.port, 8080);
}

TEST(Url, HttpsDefaultPort) {
    auto u = parse_uri("https://api.example.com");
    EXPECT_EQ(u.kind, UrlKind::Https);
    EXPECT_EQ(u.port, 8443);
}

TEST(Url, HttpExplicitPort) {
    auto u = parse_uri("https://api.example.com:9443");
    EXPECT_EQ(u.kind, UrlKind::Https);
    EXPECT_EQ(u.port, 9443);
}

TEST(Url, UserPassFromAuthority) {
    auto u = parse_uri("red://alice:hunter2@db.local:5050");
    ASSERT_TRUE(u.username);
    ASSERT_TRUE(u.password);
    EXPECT_EQ(*u.username, "alice");
    EXPECT_EQ(*u.password, "hunter2");
}

TEST(Url, UserOnlyFromAuthority) {
    auto u = parse_uri("red://alice@db.local");
    ASSERT_TRUE(u.username);
    EXPECT_FALSE(u.password.has_value());
    EXPECT_EQ(*u.username, "alice");
}

TEST(Url, PercentDecodedUserInfo) {
    auto u = parse_uri("red://al%40ice:p%40ss@db.local");
    EXPECT_EQ(*u.username, "al@ice");
    EXPECT_EQ(*u.password, "p@ss");
}

TEST(Url, TokenQueryParam) {
    auto u = parse_uri("red://db.local:5050?token=sk-abc");
    ASSERT_TRUE(u.token);
    EXPECT_EQ(*u.token, "sk-abc");
}

TEST(Url, ApiKeyAlias) {
    auto u1 = parse_uri("red://db.local?apiKey=ak-1");
    auto u2 = parse_uri("red://db.local?api_key=ak-2");
    ASSERT_TRUE(u1.api_key);
    ASSERT_TRUE(u2.api_key);
    EXPECT_EQ(*u1.api_key, "ak-1");
    EXPECT_EQ(*u2.api_key, "ak-2");
}

TEST(Url, LoginUrlAlias) {
    auto u1 = parse_uri("red://db.local?loginUrl=https://x/login");
    auto u2 = parse_uri("red://db.local?login_url=https://y/login");
    ASSERT_TRUE(u1.login_url);
    ASSERT_TRUE(u2.login_url);
    EXPECT_EQ(*u1.login_url, "https://x/login");
    EXPECT_EQ(*u2.login_url, "https://y/login");
}

TEST(Url, ProtoOverrideHttps) {
    auto u = parse_uri("red://db.local:8443?proto=https");
    EXPECT_EQ(u.kind, UrlKind::Https);
}

TEST(Url, ProtoOverrideReds) {
    auto u = parse_uri("red://db.local?proto=reds");
    EXPECT_EQ(u.kind, UrlKind::Reds);
}

TEST(Url, ProtoOverrideGrpcsBecomesReds) {
    auto u = parse_uri("red://db.local?proto=grpcs");
    EXPECT_EQ(u.kind, UrlKind::Reds);
}

TEST(Url, EmptyUriRejected) {
    EXPECT_THROW(parse_uri(""), RedDBError);
}

TEST(Url, EmbeddedRedRejected) {
    EXPECT_THROW(parse_uri("red://"), RedDBError);
}

TEST(Url, EmbeddedRedPathRejected) {
    EXPECT_THROW(parse_uri("red:///var/lib/data.rdb"), RedDBError);
}

TEST(Url, EmbeddedMemoryRejected) {
    EXPECT_THROW(parse_uri("memory://"), RedDBError);
}

TEST(Url, EmbeddedFileRejected) {
    EXPECT_THROW(parse_uri("file:///tmp/x.rdb"), RedDBError);
}

TEST(Url, UnsupportedSchemeRejected) {
    EXPECT_THROW(parse_uri("mongodb://localhost"), RedDBError);
}

TEST(Url, BadProtoRejected) {
    EXPECT_THROW(parse_uri("red://db.local?proto=ftp"), RedDBError);
}

TEST(Url, OriginalUriPreserved) {
    auto u = parse_uri("red://localhost:5050?token=abc");
    EXPECT_EQ(u.original_uri, "red://localhost:5050?token=abc");
}

TEST(Url, GrpcAliasMapsToRed) {
    auto u = parse_uri("grpc://localhost:5051");
    EXPECT_EQ(u.kind, UrlKind::Red);
    EXPECT_EQ(u.port, 5051);
}

TEST(Url, GrpcsAliasMapsToReds) {
    auto u = parse_uri("grpcs://localhost:5052");
    EXPECT_EQ(u.kind, UrlKind::Reds);
    EXPECT_EQ(u.port, 5052);
}

TEST(Url, ErrorCodeForEmbedded) {
    try {
        parse_uri("red:///abs/path");
        FAIL() << "expected EmbeddedUnsupported";
    } catch (const RedDBError& e) {
        EXPECT_EQ(e.code(), ErrorCode::EmbeddedUnsupported);
    }
}

TEST(Url, BadPortRejected) {
    EXPECT_THROW(parse_uri("red://localhost:abc"), RedDBError);
}

TEST(Url, MissingHostRejected) {
    // `red://?proto=https` — no host. raw_parse picks up empty
    // host and we throw InvalidUri.
    EXPECT_THROW(parse_uri("red://?proto=https"), RedDBError);
}

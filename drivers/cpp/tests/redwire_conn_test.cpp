// Drive RedWireConn::from_stream() through a socketpair-based fake
// server. Covers anonymous + bearer + the two AuthFail surfaces
// (at HelloAck and at AuthOk) and the bad-magic path.

#include "reddb/errors.hpp"
#include "reddb/redwire/codec.hpp"
#include "reddb/redwire/conn.hpp"
#include "reddb/redwire/frame.hpp"

#include <gtest/gtest.h>

#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

#include <atomic>
#include <cstring>
#include <future>
#include <memory>
#include <string>
#include <thread>
#include <vector>

using namespace reddb::redwire;

namespace {

// Read exactly N bytes from fd; returns false on EOF/error.
bool read_n(int fd, void* buf, size_t n) {
    auto* p = static_cast<uint8_t*>(buf);
    size_t got = 0;
    while (got < n) {
        ssize_t r = ::recv(fd, p + got, n - got, 0);
        if (r <= 0) return false;
        got += static_cast<size_t>(r);
    }
    return true;
}

bool write_n(int fd, const void* buf, size_t n) {
    auto* p = static_cast<const uint8_t*>(buf);
    size_t sent = 0;
    while (sent < n) {
        ssize_t r = ::send(fd, p + sent, n - sent, 0);
        if (r <= 0) return false;
        sent += static_cast<size_t>(r);
    }
    return true;
}

bool read_frame_fd(int fd, Frame& out) {
    uint8_t header[FRAME_HEADER_SIZE];
    if (!read_n(fd, header, FRAME_HEADER_SIZE)) return false;
    FrameHeader h = decode_header(header);
    if (h.length < FRAME_HEADER_SIZE || h.length > MAX_FRAME_SIZE) return false;
    std::vector<uint8_t> buf(h.length);
    std::memcpy(buf.data(), header, FRAME_HEADER_SIZE);
    if (h.length > FRAME_HEADER_SIZE) {
        if (!read_n(fd, buf.data() + FRAME_HEADER_SIZE, h.length - FRAME_HEADER_SIZE)) return false;
    }
    auto [f, _n] = decode_frame(buf.data(), buf.size());
    out = f;
    return true;
}

bool write_frame_fd(int fd, const Frame& f) {
    auto bytes = encode_frame(f);
    return write_n(fd, bytes.data(), bytes.size());
}

// Fake-server helper. Reads magic bytes + Hello + AuthResponse
// then writes the frames the test scripted.
struct FakeServer {
    int server_fd = -1;
    int client_fd = -1;
    std::thread th;

    FakeServer() = default;
    FakeServer(const FakeServer&) = delete;
    FakeServer& operator=(const FakeServer&) = delete;
    FakeServer(FakeServer&&) = default;
    FakeServer& operator=(FakeServer&&) = default;

    static std::unique_ptr<FakeServer> make() {
        int sv[2] = {-1, -1};
        ::socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
        auto fs = std::make_unique<FakeServer>();
        fs->client_fd = sv[0];
        fs->server_fd = sv[1];
        return fs;
    }

    ~FakeServer() {
        if (th.joinable()) th.join();
        if (server_fd >= 0) ::close(server_fd);
        // client_fd is owned by the IoStream we hand to RedWireConn.
    }
};

} // namespace

TEST(RedWireConn, AnonymousHandshakeSucceeds) {
    auto fs = FakeServer::make();

    // Server: read magic+ver, read Hello, send HelloAck (auth=anonymous),
    // read AuthResponse (empty), send AuthOk.
    fs->th = std::thread([fd = fs->server_fd]() {
        uint8_t magic[2];
        ASSERT_TRUE(read_n(fd, magic, 2));
        ASSERT_EQ(magic[0], MAGIC);
        ASSERT_EQ(magic[1], SUPPORTED_VERSION);

        Frame hello;
        ASSERT_TRUE(read_frame_fd(fd, hello));
        ASSERT_EQ(hello.kind, MessageKind::Hello);

        Frame ack;
        ack.kind = MessageKind::HelloAck;
        ack.correlation_id = hello.correlation_id;
        std::string body = R"({"version":1,"auth":"anonymous","features":0})";
        ack.payload.assign(body.begin(), body.end());
        ASSERT_TRUE(write_frame_fd(fd, ack));

        Frame resp;
        ASSERT_TRUE(read_frame_fd(fd, resp));
        ASSERT_EQ(resp.kind, MessageKind::AuthResponse);

        Frame ok;
        ok.kind = MessageKind::AuthOk;
        ok.correlation_id = resp.correlation_id;
        std::string ob = R"({"session_id":"sess-1","features":0})";
        ok.payload.assign(ob.begin(), ob.end());
        ASSERT_TRUE(write_frame_fd(fd, ok));
    });

    auto stream = wrap_fd(fs->client_fd);
    fs->client_fd = -1;
    ConnectOpts opts;
    opts.host = "fake";
    opts.auth.method = AuthMethod::Anonymous;
    auto conn = RedWireConn::from_stream(std::move(stream), opts);
    EXPECT_EQ(conn->session_id(), "sess-1");
}

TEST(RedWireConn, BearerHandshakeSucceeds) {
    auto fs = FakeServer::make();
    fs->th = std::thread([fd = fs->server_fd]() {
        uint8_t magic[2];
        ASSERT_TRUE(read_n(fd, magic, 2));
        Frame hello; ASSERT_TRUE(read_frame_fd(fd, hello));

        Frame ack;
        ack.kind = MessageKind::HelloAck;
        ack.correlation_id = hello.correlation_id;
        std::string body = R"({"version":1,"auth":"bearer","features":0})";
        ack.payload.assign(body.begin(), body.end());
        ASSERT_TRUE(write_frame_fd(fd, ack));

        Frame resp;
        ASSERT_TRUE(read_frame_fd(fd, resp));
        ASSERT_EQ(resp.kind, MessageKind::AuthResponse);
        std::string sent(resp.payload.begin(), resp.payload.end());
        ASSERT_NE(sent.find("\"sk-secret\""), std::string::npos);

        Frame ok;
        ok.kind = MessageKind::AuthOk;
        ok.correlation_id = resp.correlation_id;
        std::string ob = R"({"session_id":"sess-bearer","features":1})";
        ok.payload.assign(ob.begin(), ob.end());
        ASSERT_TRUE(write_frame_fd(fd, ok));
    });

    auto stream = wrap_fd(fs->client_fd);
    fs->client_fd = -1;
    ConnectOpts opts;
    opts.host = "fake";
    opts.auth.method = AuthMethod::Bearer;
    opts.auth.token = "sk-secret";
    auto conn = RedWireConn::from_stream(std::move(stream), opts);
    EXPECT_EQ(conn->session_id(), "sess-bearer");
    EXPECT_EQ(conn->server_features(), 1u);
}

TEST(RedWireConn, AuthFailAtHelloAck) {
    auto fs = FakeServer::make();
    fs->th = std::thread([fd = fs->server_fd]() {
        uint8_t magic[2];
        ASSERT_TRUE(read_n(fd, magic, 2));
        Frame hello; ASSERT_TRUE(read_frame_fd(fd, hello));

        Frame fail;
        fail.kind = MessageKind::AuthFail;
        fail.correlation_id = hello.correlation_id;
        std::string body = R"({"reason":"no compatible auth method"})";
        fail.payload.assign(body.begin(), body.end());
        ASSERT_TRUE(write_frame_fd(fd, fail));
    });

    auto stream = wrap_fd(fs->client_fd);
    fs->client_fd = -1;
    ConnectOpts opts;
    opts.host = "fake";
    opts.auth.method = AuthMethod::Anonymous;
    try {
        auto conn = RedWireConn::from_stream(std::move(stream), opts);
        FAIL() << "expected AuthRefused";
    } catch (const reddb::RedDBError& e) {
        EXPECT_EQ(e.code(), reddb::ErrorCode::AuthRefused);
        EXPECT_NE(std::string(e.what()).find("no compatible auth method"), std::string::npos);
    }
}

TEST(RedWireConn, AuthFailAtAuthOk) {
    auto fs = FakeServer::make();
    fs->th = std::thread([fd = fs->server_fd]() {
        uint8_t magic[2];
        ASSERT_TRUE(read_n(fd, magic, 2));
        Frame hello; ASSERT_TRUE(read_frame_fd(fd, hello));

        Frame ack;
        ack.kind = MessageKind::HelloAck;
        ack.correlation_id = hello.correlation_id;
        std::string body = R"({"version":1,"auth":"bearer","features":0})";
        ack.payload.assign(body.begin(), body.end());
        ASSERT_TRUE(write_frame_fd(fd, ack));

        Frame resp;
        ASSERT_TRUE(read_frame_fd(fd, resp));

        Frame fail;
        fail.kind = MessageKind::AuthFail;
        fail.correlation_id = resp.correlation_id;
        std::string fb = R"({"reason":"bearer token invalid"})";
        fail.payload.assign(fb.begin(), fb.end());
        ASSERT_TRUE(write_frame_fd(fd, fail));
    });

    auto stream = wrap_fd(fs->client_fd);
    fs->client_fd = -1;
    ConnectOpts opts;
    opts.host = "fake";
    opts.auth.method = AuthMethod::Bearer;
    opts.auth.token = "bogus";
    try {
        auto conn = RedWireConn::from_stream(std::move(stream), opts);
        FAIL() << "expected AuthRefused";
    } catch (const reddb::RedDBError& e) {
        EXPECT_EQ(e.code(), reddb::ErrorCode::AuthRefused);
        EXPECT_NE(std::string(e.what()).find("bearer token invalid"), std::string::npos);
    }
}

TEST(RedWireConn, BadMagicSurfacesAsProtocolError) {
    // The server peer reads the 2 magic bytes from the client and
    // immediately closes — simulating a non-RedWire endpoint that
    // discarded our handshake. The client should fail when it
    // tries to read the HelloAck.
    auto fs = FakeServer::make();
    fs->th = std::thread([fd = fs->server_fd]() {
        uint8_t magic[2];
        ASSERT_TRUE(read_n(fd, magic, 2));
        Frame hello;
        // Drain the Hello frame too, then close.
        (void)read_frame_fd(fd, hello);
        ::shutdown(fd, SHUT_RDWR);
    });

    auto stream = wrap_fd(fs->client_fd);
    fs->client_fd = -1;
    ConnectOpts opts;
    opts.host = "fake";
    opts.auth.method = AuthMethod::Anonymous;
    try {
        auto conn = RedWireConn::from_stream(std::move(stream), opts);
        FAIL() << "expected error";
    } catch (const reddb::RedDBError& e) {
        // Either Network (closed mid-frame) or Protocol (truncated)
        EXPECT_TRUE(e.code() == reddb::ErrorCode::Network ||
                    e.code() == reddb::ErrorCode::Protocol);
    }
}

TEST(RedWireConn, QueryRoundTrip) {
    auto fs = FakeServer::make();
    fs->th = std::thread([fd = fs->server_fd]() {
        uint8_t magic[2];
        ASSERT_TRUE(read_n(fd, magic, 2));
        Frame hello; ASSERT_TRUE(read_frame_fd(fd, hello));

        Frame ack;
        ack.kind = MessageKind::HelloAck;
        ack.correlation_id = hello.correlation_id;
        std::string body = R"({"version":1,"auth":"anonymous","features":0})";
        ack.payload.assign(body.begin(), body.end());
        ASSERT_TRUE(write_frame_fd(fd, ack));

        Frame resp; ASSERT_TRUE(read_frame_fd(fd, resp));

        Frame ok;
        ok.kind = MessageKind::AuthOk;
        ok.correlation_id = resp.correlation_id;
        std::string ob = R"({"session_id":"s","features":0})";
        ok.payload.assign(ob.begin(), ob.end());
        ASSERT_TRUE(write_frame_fd(fd, ok));

        Frame qf;
        ASSERT_TRUE(read_frame_fd(fd, qf));
        ASSERT_EQ(qf.kind, MessageKind::Query);
        std::string sql(qf.payload.begin(), qf.payload.end());
        ASSERT_EQ(sql, "SELECT 1");

        Frame result;
        result.kind = MessageKind::Result;
        result.correlation_id = qf.correlation_id;
        std::string rb = R"({"ok":true,"rows":[]})";
        result.payload.assign(rb.begin(), rb.end());
        ASSERT_TRUE(write_frame_fd(fd, result));
    });

    auto stream = wrap_fd(fs->client_fd);
    fs->client_fd = -1;
    ConnectOpts opts;
    opts.host = "fake";
    opts.auth.method = AuthMethod::Anonymous;
    auto conn = RedWireConn::from_stream(std::move(stream), opts);
    auto json = conn->query("SELECT 1");
    EXPECT_EQ(json, R"({"ok":true,"rows":[]})");
}

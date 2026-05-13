// End-to-end smoke. Off by default; enable with RED_SMOKE=1. The
// test spawns a real RedDB binary (path via RED_BIN or RED_BINARY)
// and exercises parameterized RedWire queries against it.

#include "reddb/reddb.hpp"

#include <gtest/gtest.h>

#include <sys/types.h>
#include <sys/wait.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <signal.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cerrno>
#include <chrono>
#include <cstdlib>
#include <cstring>
#include <array>
#include <string>
#include <thread>
#include <memory>

namespace {

bool smoke_enabled() {
    const char* env = std::getenv("RED_SMOKE");
    return env && std::string(env) == "1";
}

struct ChildBinary {
    pid_t pid = -1;
    ~ChildBinary() {
        if (pid > 0) {
            ::kill(pid, SIGTERM);
            int status = 0;
            ::waitpid(pid, &status, 0);
        }
    }
};

std::string getenv_string(const char* key) {
    const char* value = std::getenv(key);
    return value ? std::string(value) : std::string();
}

int pick_free_port() {
    int fd = ::socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    sockaddr_in addr{};
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = 0;
    if (::bind(fd, reinterpret_cast<sockaddr*>(&addr), sizeof(addr)) != 0) {
        ::close(fd);
        return -1;
    }
    socklen_t len = sizeof(addr);
    if (::getsockname(fd, reinterpret_cast<sockaddr*>(&addr), &len) != 0) {
        ::close(fd);
        return -1;
    }
    int port = ntohs(addr.sin_port);
    ::close(fd);
    return port;
}

std::unique_ptr<reddb::Conn> wait_for_connect(const std::string& uri) {
    auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(60);
    while (std::chrono::steady_clock::now() < deadline) {
        try {
            auto conn = reddb::connect(uri);
            conn->ping();
            return conn;
        } catch (const reddb::RedDBError&) {
            std::this_thread::sleep_for(std::chrono::milliseconds(50));
        }
    }
    return nullptr;
}

} // namespace

TEST(Smoke, FullRoundTrip) {
    if (!smoke_enabled()) {
        GTEST_SKIP() << "RED_SMOKE not set; skipping (set RED_SMOKE=1 to enable)";
    }
    std::string binary = getenv_string("RED_BIN");
    if (binary.empty()) binary = getenv_string("RED_BINARY");
    if (binary.empty()) {
        GTEST_SKIP() << "set RED_BIN=/path/to/red to enable the engine smoke";
    }

    int picked_port = pick_free_port();
    ASSERT_GT(picked_port, 0) << "could not allocate free port";
    std::string port = std::to_string(picked_port);
    char tmp_template[] = "/tmp/reddb-cpp-smoke-XXXXXX";
    char* tmp_dir = ::mkdtemp(tmp_template);
    ASSERT_NE(tmp_dir, nullptr) << "mkdtemp failed: " << std::strerror(errno);
    std::string data_path = std::string(tmp_dir) + "/data.db";

    ChildBinary cb;
    cb.pid = ::fork();
    ASSERT_GE(cb.pid, 0) << "fork failed: " << std::strerror(errno);
    if (cb.pid == 0) {
        std::string addr = "127.0.0.1:" + port;
        const char* args[] = {
            binary.c_str(), "server",
            "--path", data_path.c_str(),
            "--bind", addr.c_str(),
            nullptr
        };
        ::execvp(binary.c_str(), const_cast<char**>(args));
        std::perror("execvp");
        ::_exit(127);
    }

    std::string uri = "red://127.0.0.1:" + port;
    std::unique_ptr<reddb::Conn> conn = wait_for_connect(uri);
    ASSERT_NE(conn, nullptr) << "could not connect to " << uri;

    auto json = conn->query("SELECT 1");
    EXPECT_FALSE(json.empty());

    conn->ping();

    conn->query("CREATE TABLE cpp_params (id INT, name TEXT)");
    std::array<reddb::Value, 2> insert_params = {reddb::Value::int64(42), reddb::Value("alice")};
    conn->query("INSERT INTO cpp_params (id, name) VALUES ($1, $2)", insert_params);
    std::array<reddb::Value, 2> filters = {reddb::Value::int64(42), reddb::Value("alice")};
    auto param_got = conn->query("SELECT name FROM cpp_params WHERE id = $1 AND name = $2", filters);
    EXPECT_NE(param_got.find("alice"), std::string::npos) << param_got;

    std::array<float, 2> vector = {0.7f, 0.7f};
    std::array<reddb::Value, 2> vector_insert = {
        reddb::Value::vector(vector),
        reddb::Value("parameterized doc"),
    };
    conn->query("INSERT INTO smoke_embeddings VECTOR (dense, content) VALUES ($1, $2)",
                vector_insert);
    std::array<reddb::Value, 1> vector_search = {reddb::Value::vector(vector)};
    auto vector_got = conn->query("SEARCH SIMILAR $1 COLLECTION smoke_embeddings LIMIT 1",
                                  vector_search);
    EXPECT_NE(vector_got.find("\"record_count\":1"), std::string::npos) << vector_got;

    conn->close();
}

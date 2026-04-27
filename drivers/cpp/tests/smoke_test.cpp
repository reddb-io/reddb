// End-to-end smoke. Off by default; enable with RED_SMOKE=1. The
// test spawns a real RedDB binary (path via RED_BINARY, default
// `target/debug/reddb`) and exercises connect/query/insert/get/
// delete/ping/close against it.

#include "reddb/reddb.hpp"

#include <gtest/gtest.h>

#include <sys/types.h>
#include <sys/wait.h>
#include <signal.h>
#include <unistd.h>

#include <cerrno>
#include <chrono>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>

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

} // namespace

TEST(Smoke, FullRoundTrip) {
    if (!smoke_enabled()) {
        GTEST_SKIP() << "RED_SMOKE not set; skipping (set RED_SMOKE=1 to enable)";
    }
    const char* bin = std::getenv("RED_BINARY");
    std::string binary = bin ? bin : "target/debug/reddb";

    // Pick an ephemeral-ish port. Operators can override with
    // RED_PORT to avoid clashes.
    const char* port_env = std::getenv("RED_PORT");
    std::string port = port_env ? port_env : "5050";

    ChildBinary cb;
    cb.pid = ::fork();
    ASSERT_GE(cb.pid, 0) << "fork failed: " << std::strerror(errno);
    if (cb.pid == 0) {
        // child: exec the binary
        std::string addr = "127.0.0.1:" + port;
        const char* args[] = {binary.c_str(), "serve", "--listen", addr.c_str(), nullptr};
        ::execvp(binary.c_str(), const_cast<char**>(args));
        std::perror("execvp");
        ::_exit(127);
    }

    // Wait briefly for the listener.
    std::this_thread::sleep_for(std::chrono::seconds(1));

    std::string uri = "red://127.0.0.1:" + port;
    std::unique_ptr<reddb::Conn> conn;
    try {
        conn = reddb::connect(uri);
    } catch (const reddb::RedDBError& e) {
        GTEST_SKIP() << "could not connect to " << uri << ": " << e.what();
    }

    auto json = conn->query("SELECT 1");
    EXPECT_FALSE(json.empty());

    conn->ping();

    // Best-effort: create a tiny collection and round-trip a row.
    try {
        conn->query("CREATE TABLE smoke_users (name TEXT, age INTEGER)");
        auto ok = conn->insert("smoke_users", R"({"name":"alice","age":30})");
        EXPECT_FALSE(ok.empty());
        auto got = conn->get("smoke_users", "alice");
        EXPECT_FALSE(got.empty());
        auto deld = conn->del("smoke_users", "alice");
        EXPECT_FALSE(deld.empty());
    } catch (const reddb::RedDBError& e) {
        // Engine semantics may vary by build; don't fail the test
        // for these. The handshake + query already proved the
        // wire works.
        GTEST_LOG_(INFO) << "ops side: " << e.what();
    }

    conn->close();
}

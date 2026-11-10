#include "reddb/errors.hpp"
#include "reddb/reddb.hpp"

#include <gtest/gtest.h>

#include <chrono>
#include <filesystem>
#include <string>

namespace {

class TempDirectory {
public:
    TempDirectory() {
        const auto nonce = std::chrono::steady_clock::now().time_since_epoch().count();
        path = std::filesystem::temp_directory_path() /
               ("reddb-cpp-embedded-" + std::to_string(nonce));
        std::filesystem::create_directories(path);
    }

    ~TempDirectory() {
        std::error_code error;
        std::filesystem::remove_all(path, error);
    }

    std::filesystem::path path;
};

std::string file_uri(const std::filesystem::path& path) {
#ifdef _WIN32
    return "file:///" + path.generic_string();
#else
    return "file://" + path.generic_string();
#endif
}

std::string json_string_field(const std::string& json, const std::string& field) {
    const std::string prefix = "\"" + field + "\":\"";
    const size_t start = json.find(prefix);
    if (start == std::string::npos) return {};
    const size_t value_start = start + prefix.size();
    const size_t end = json.find('"', value_start);
    if (end == std::string::npos) return {};
    return json.substr(value_start, end - value_start);
}

} // namespace

TEST(Embedded, MemorySupportsParameterizedQueries) {
    auto conn = reddb::connect("memory://");
    conn->query("CREATE TABLE cpp_items (id INTEGER, name TEXT)");
    conn->query("INSERT INTO cpp_items (id, name) VALUES ($1, $2)",
                {reddb::Value::int64(7), reddb::Value("embedded")});
    const std::string rows = conn->query("SELECT id, name FROM cpp_items WHERE id = $1",
                                         {reddb::Value::int64(7)});
    EXPECT_NE(rows.find("embedded"), std::string::npos);
    conn->ping();
    conn->close();
}

TEST(Embedded, CrudMethodsUseTheCanonicalDispatcher) {
    auto conn = reddb::connect("memory://");
    const std::string inserted =
        conn->insert("cpp_entities", R"({"name":"alice","age":30})");
    const std::string id = json_string_field(inserted, "id");
    ASSERT_FALSE(id.empty()) << inserted;
    EXPECT_NE(conn->get("cpp_entities", id).find("alice"), std::string::npos);

    const std::string bulk = conn->bulk_insert(
        "cpp_entities", {R"({"name":"bob","age":31})", R"({"name":"carol","age":32})"});
    EXPECT_NE(bulk.find("\"affected\":2"), std::string::npos);
    EXPECT_NE(conn->query("SELECT * FROM cpp_entities").find("carol"), std::string::npos);

    EXPECT_NE(conn->del("cpp_entities", id).find("\"affected\":1"), std::string::npos);
    EXPECT_NE(conn->get("cpp_entities", id).find("\"entity\":null"), std::string::npos);
    conn->close();
}

TEST(Embedded, PersistentDatabaseReopensAsExactlyOneFile) {
    TempDirectory temp;
    const std::filesystem::path database = temp.path / "cpp-contract.rdb";
    const std::string uri = file_uri(database);

    {
        auto conn = reddb::connect(uri);
        conn->query("CREATE TABLE cpp_persist (id INTEGER, name TEXT)");
        conn->query("INSERT INTO cpp_persist (id, name) VALUES (9, 'survives')");
        conn->close();
    }
    {
        auto conn = reddb::connect(uri);
        const std::string rows = conn->query("SELECT id, name FROM cpp_persist WHERE id = 9");
        EXPECT_NE(rows.find("survives"), std::string::npos);
        conn->close();
    }

    size_t artifact_count = 0;
    std::filesystem::path only_artifact;
    for (const auto& entry : std::filesystem::directory_iterator(temp.path)) {
        ++artifact_count;
        only_artifact = entry.path();
    }
    EXPECT_EQ(artifact_count, 1U);
    EXPECT_EQ(only_artifact, database);
}

TEST(Embedded, RejectsAConcurrentWriterForTheSameFile) {
    TempDirectory temp;
    const std::string uri = file_uri(temp.path / "writer.rdb");
    auto first = reddb::connect(uri);
    try {
        auto second = reddb::connect(uri);
        FAIL() << "expected the second writer to be rejected";
    } catch (const reddb::RedDBError& error) {
        EXPECT_EQ(error.code(), reddb::ErrorCode::Engine);
    }
    first->close();
}

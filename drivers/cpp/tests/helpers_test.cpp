// Mirrors `drivers/go/helpers_test.go` 1:1 via GoogleTest + a FakeQuerier
// that records SQL and replays scripted JSON responses.

#include "reddb/helpers.hpp"

#include <gtest/gtest.h>

#include <deque>
#include <stdexcept>
#include <string>
#include <vector>

using namespace reddb::helpers;

namespace {

struct FakeCall {
    std::string sql;
    std::vector<std::string> params;
};

class FakeQuerier : public IQuerier {
public:
    std::vector<FakeCall> calls;
    std::deque<std::string> replies;
    std::deque<std::string> errs;  // empty string = no error

    std::string query(const std::string& sql,
                      const std::vector<std::string>& params) override {
        calls.push_back({sql, params});
        if (!errs.empty()) {
            std::string e = errs.front();
            errs.pop_front();
            if (!e.empty()) throw std::runtime_error(e);
        }
        if (replies.empty()) return std::string{};
        std::string body = replies.front();
        replies.pop_front();
        return body;
    }
};

std::shared_ptr<FakeQuerier> make_fake() { return std::make_shared<FakeQuerier>(); }

bool contains(const std::string& haystack, const std::string& needle) {
    return haystack.find(needle) != std::string::npos;
}

} // namespace

// ---------------------------------------------------------------- KV path

TEST(KVPath, QuotesNamespacedKeys) {
    EXPECT_EQ(sql::kv_path("kv_default", "corpus:version"),
              "kv_default.'corpus:version'");
}

TEST(KVPath, PreservesDotsAndSlashes) {
    EXPECT_EQ(sql::kv_path("kv_default", "a/b.c"), "kv_default.'a/b.c'");
}

TEST(KVPath, RejectsBadCollection) {
    try {
        sql::kv_path("bad-name!", "k");
        FAIL();
    } catch (const HelperError& e) {
        EXPECT_EQ(e.code(), HelperError::Code::InvalidArgument);
    }
}

TEST(KVValueLiteral, BasicCases) {
    EXPECT_EQ(sql::kv_value_literal(JsonValue::Null()), "NULL");
    EXPECT_EQ(sql::kv_value_literal(JsonValue::make_bool(true)), "true");
    EXPECT_EQ(sql::kv_value_literal(JsonValue::make_bool(false)), "false");
    EXPECT_EQ(sql::kv_value_literal(JsonValue::make_int(42)), "42");
    EXPECT_EQ(sql::kv_value_literal(JsonValue::make_string("hi")), "'hi'");
    EXPECT_EQ(sql::kv_value_literal(JsonValue::make_string("o'reilly")),
              "'o''reilly'");
    JsonObject obj;
    obj.emplace("a", JsonValue::make_int(1));
    EXPECT_EQ(sql::kv_value_literal(JsonValue::make_object(obj)),
              "'{\"a\":1}'");
}

// ---------------------------------------------------------------- KV ops

TEST(KV, SetEmitsExactKeyPath) {
    auto fq = make_fake();
    fq->replies.push_back("{}");
    Helpers h(fq);
    h.kv().set("characters:hansel", JsonValue::make_string("ok"));
    const std::string& sql = fq->calls[0].sql;
    EXPECT_TRUE(contains(sql, "kv_default.'characters:hansel'")) << sql;
    EXPECT_TRUE(contains(sql, "= 'ok'")) << sql;
}

TEST(KV, GetReturnsValueOrNull) {
    auto fq = make_fake();
    fq->replies.push_back(R"({"rows":[{"value":"v"}]})");
    fq->replies.push_back(R"({"rows":[]})");
    Helpers h(fq);
    JsonValue got = h.kv().get("k");
    ASSERT_TRUE(got.is_string());
    EXPECT_EQ(got.as_string(), "v");
    JsonValue got2 = h.kv().get("k2");
    EXPECT_TRUE(got2.is_null());
}

TEST(KV, ExistsUsesGet) {
    auto fq = make_fake();
    fq->replies.push_back(R"({"rows":[{"value":"v"}]})");
    fq->replies.push_back(R"({"rows":[]})");
    Helpers h(fq);
    EXPECT_TRUE(h.kv().exists("k").exists);
    EXPECT_FALSE(h.kv().exists("k2").exists);
}

TEST(KV, ListFiltersByPrefixWithoutRewriting) {
    auto fq = make_fake();
    fq->replies.push_back(R"({"rows":[
        {"key":"a:1","value":1},
        {"key":"b:1","value":2},
        {"key":"a:2","value":3}
    ]})");
    Helpers h(fq);
    KvClient::ListOpts opts;
    opts.prefix = "a:";
    auto out = h.kv().list(opts);
    ASSERT_EQ(out.items.size(), 2u);
    EXPECT_EQ(out.items[0].at("key").as_string(), "a:1");
    EXPECT_EQ(out.items[1].at("key").as_string(), "a:2");
}

TEST(KV, ListRejectsNegativeLimit) {
    auto fq = make_fake();
    Helpers h(fq);
    KvClient::ListOpts opts;
    opts.limit = -1;
    try {
        h.kv().list(opts);
        FAIL();
    } catch (const HelperError& e) {
        EXPECT_EQ(e.code(), HelperError::Code::InvalidArgument);
    }
}

// ---------------------------------------------------------------- Queue

TEST(Queue, PushEmitsPriorityAndPayload) {
    auto fq = make_fake();
    fq->replies.push_back(R"({"affected":1})");
    Helpers h(fq);
    JsonObject payload;
    payload.emplace("id", JsonValue::make_int(1));
    QueueClient::PushOptions opts;
    opts.priority = 5;
    h.queue().push("jobs", JsonValue::make_object(payload), opts);
    const std::string& sql = fq->calls[0].sql;
    EXPECT_EQ(sql.rfind("QUEUE PUSH jobs ", 0), 0u) << sql;
    EXPECT_TRUE(contains(sql, "PRIORITY 5")) << sql;
    EXPECT_TRUE(contains(sql, "{\"id\":1}")) << sql;
}

TEST(Queue, LenReturnsInt) {
    auto fq = make_fake();
    fq->replies.push_back(R"({"rows":[{"len":3}]})");
    Helpers h(fq);
    EXPECT_EQ(h.queue().len("jobs"), 3);
}

TEST(Queue, PopReturnsPayloads) {
    auto fq = make_fake();
    fq->replies.push_back(R"({"rows":[{"payload":"a"},{"payload":"b"}]})");
    Helpers h(fq);
    auto out = h.queue().pop("jobs", 2);
    ASSERT_EQ(out.size(), 2u);
    EXPECT_EQ(out[0].as_string(), "a");
    EXPECT_EQ(out[1].as_string(), "b");
}

TEST(Queue, PopRejectsNegativeCount) {
    auto fq = make_fake();
    Helpers h(fq);
    try {
        h.queue().pop("jobs", -1);
        FAIL();
    } catch (const HelperError& e) {
        EXPECT_EQ(e.code(), HelperError::Code::InvalidArgument);
    }
}

TEST(Queue, PushRejectsInvalidIdentifier) {
    auto fq = make_fake();
    Helpers h(fq);
    try {
        h.queue().push("bad-name!", JsonValue::make_string("x"));
        FAIL();
    } catch (const HelperError& e) {
        EXPECT_EQ(e.code(), HelperError::Code::InvalidArgument);
    }
}

// ---------------------------------------------------------------- Documents

TEST(Documents, InsertReturnsRIDEnvelope) {
    auto fq = make_fake();
    fq->replies.push_back(R"({"rows":[],"affected":0})");
    fq->replies.push_back(
        R"({"rows":[{"rid":"doc-1","body":{"name":"alice"}}],"affected":1})");
    Helpers h(fq);
    JsonObject doc;
    doc.emplace("name", JsonValue::make_string("alice"));
    auto out = h.documents().insert("people", doc);
    EXPECT_EQ(out.affected, 1);
    EXPECT_EQ(out.rid, "doc-1");
    ASSERT_TRUE(out.item.has_value());
    EXPECT_EQ(out.item->at("rid").as_string(), "doc-1");
}

TEST(Documents, GetRaisesNotFoundOnMissing) {
    auto fq = make_fake();
    fq->replies.push_back(R"({"rows":[]})");
    Helpers h(fq);
    try {
        h.documents().get("people", "doc-1");
        FAIL();
    } catch (const HelperError& e) {
        EXPECT_EQ(e.code(), HelperError::Code::NotFound);
    }
}

TEST(Documents, PatchRejectsJSONPointerPaths) {
    auto fq = make_fake();
    Helpers h(fq);
    JsonObject patch;
    patch.emplace("a/b", JsonValue::make_int(1));
    try {
        h.documents().patch("people", "doc-1", patch);
        FAIL();
    } catch (const HelperError& e) {
        EXPECT_EQ(e.code(), HelperError::Code::InvalidArgument);
    }
}

TEST(Documents, ListOrdersByRIDByDefault) {
    auto fq = make_fake();
    fq->replies.push_back(R"({"rows":[{"rid":"a"},{"rid":"b"}]})");
    Helpers h(fq);
    auto out = h.documents().list("people");
    EXPECT_EQ(out.items.size(), 2u);
    EXPECT_TRUE(contains(fq->calls[0].sql, "ORDER BY rid ASC")) << fq->calls[0].sql;
}

TEST(Documents, InsertPassesThroughExistingCollection) {
    auto fq = make_fake();
    fq->replies.push_back("{}");
    fq->replies.push_back(R"({"rows":[{"rid":"x"}],"affected":1})");
    fq->errs.push_back("collection already exists");
    fq->errs.push_back("");
    Helpers h(fq);
    JsonObject doc;
    doc.emplace("a", JsonValue::make_int(1));
    auto out = h.documents().insert("people", doc);
    EXPECT_EQ(out.rid, "x");
}

// ---------------------------------------------------------- decode helpers

TEST(Decode, AffectedFromBodyHandlesNestedResult) {
    std::string body = R"({"result":{"affected":7}})";
    EXPECT_EQ(sql::affected_from_body(body), 7);
}

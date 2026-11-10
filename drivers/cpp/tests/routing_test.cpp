#include "../src/routing.hpp"
#include "reddb/helpers.hpp"
#include "reddb/redwire/conn.hpp"
#include <gtest/gtest.h>
#include <fstream>
#include <sstream>
#include <filesystem>
#include <thread>
#include <sys/socket.h>
#include <netinet/in.h>
#include <unistd.h>
using namespace reddb;
static helpers::JsonValue fixture(const std::string& name) {
    auto root=std::filesystem::path(__FILE__).parent_path().parent_path().parent_path().parent_path();
    std::ifstream input(root / "testdata/conformance/topology" / name);
    std::stringstream contents; contents<<input.rdbuf();
    return helpers::parse_json_bounded(contents.str());
}
TEST(Routing, CanonicalFixtures) {
    auto manifest=fixture("manifest.json");
    for(const auto& entry:manifest.find("cases")->as_array()) {
        const auto& hex=entry.find("hex")->as_string();std::vector<uint8_t> bytes;
        for(size_t i=0;i<hex.size();i+=2) bytes.push_back(static_cast<uint8_t>(std::stoul(hex.substr(i,2),nullptr,16)));
        if(entry.find("error") && entry.find("error")->as_bool()) { EXPECT_THROW(routing::decode_topology(bytes),RedDBError);continue; }
        auto actual=routing::decode_topology(bytes);auto expected=entry.find("expected");
        if(expected->is_null()) { EXPECT_FALSE(actual);continue; }
        ASSERT_TRUE(actual);EXPECT_EQ(std::to_string(actual->epoch),expected->find("epoch")->as_string());
        EXPECT_EQ(actual->primary.addr,expected->find("primary")->find("addr")->as_string());
        auto& replicas=expected->find("replicas")->as_array();ASSERT_EQ(actual->replicas.size(),replicas.size());
        for(size_t i=0;i<replicas.size();++i) {
            EXPECT_EQ(actual->replicas[i].endpoint.addr,replicas[i].find("addr")->as_string());
            EXPECT_EQ(std::to_string(actual->replicas[i].last_applied_lsn),replicas[i].find("last_applied_lsn")->as_string());
            EXPECT_EQ(actual->replicas[i].rebootstrapping,replicas[i].find("rebootstrapping")->as_bool());
        }
    }
    auto policy=fixture("sql-policy.json");
    for(const auto& entry:policy.as_array()) EXPECT_EQ(routing::sql_policy(entry.find("sql")->as_string()).second,entry.find("replica_read")->as_bool())<<entry.find("sql")->as_string();
}
class Peer final:public Conn {
public:
    std::string address;std::vector<std::string>& calls;Topology membership;
    Peer(std::string a,std::vector<std::string>& c,Topology t):address(a),calls(c),membership(t){}
    std::optional<Topology> topology(const std::string&) override {return membership;}
    std::string query(const std::string& sql) override {
        calls.push_back(address+" "+sql);
        if(sql.find("lost")!=std::string::npos || sql.find("effectful")!=std::string::npos) throw RedDBError(ErrorCode::Network,"lost reply");
        return address;
    }
    std::string insert(const std::string&,const std::string&) override{return query("insert");}
    std::string bulk_insert(const std::string&,const std::vector<std::string>&) override{return query("bulk");}
    std::string get(const std::string&,const std::string&) override{return query("get");}
    std::string del(const std::string&,const std::string&) override{return query("delete");}
    void ping() override{} void close() override{}
};
TEST(Routing, RoutesPinsAndNeverReplaysEffects) {
    std::string primary="https://primary:5000",replica="https://replica:5000";std::vector<std::string> calls;
    Topology membership;membership.primary.addr=primary;
    membership.replicas.push_back({{replica,""},true,10,0,false});
    auto dial=[&](const std::string& a){return std::make_unique<Peer>(a,calls,membership);};
    auto client=routing::connect(primary,dial(primary),"https",false,dial);
    EXPECT_EQ(client->query("SELECT 1"),primary);
    EXPECT_EQ(client->query_with_options("SELECT 1",{ReadConsistency::Local}),replica);
    client->query("BEGIN");EXPECT_EQ(client->query_with_options("SELECT 1",{ReadConsistency::Local}),primary);client->query("ROLLBACK");
    for(const std::string sql:{"UPDATE lost","SELECT \"effectful\"()"}) {
        auto before=calls.size();EXPECT_THROW(client->query_with_options(sql,{ReadConsistency::Local}),RedDBError);EXPECT_EQ(calls.size(),before+1);EXPECT_TRUE(calls.back().starts_with(primary));
    }
}
TEST(Routing, SilentSocketHasReadDeadline) {
    int listener=socket(AF_INET,SOCK_STREAM,0);ASSERT_GE(listener,0);
    sockaddr_in address{};address.sin_family=AF_INET;address.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
    ASSERT_EQ(bind(listener,reinterpret_cast<sockaddr*>(&address),sizeof(address)),0);ASSERT_EQ(listen(listener,1),0);
    socklen_t size=sizeof(address);getsockname(listener,reinterpret_cast<sockaddr*>(&address),&size);
    std::thread server([&]{int fd=accept(listener,nullptr,nullptr);char byte;recv(fd,&byte,1,0);close(fd);});
    auto stream=redwire::dial_tcp("127.0.0.1",ntohs(address.sin_port),50);
    uint8_t byte=0;EXPECT_THROW(stream->read_exact(&byte,1),RedDBError);stream->close();server.join();close(listener);
}

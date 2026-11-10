#pragma once
#include <chrono>
#include <cstdint>
#include <string>
#include <vector>

namespace reddb {
enum class ReadConsistency { Strong, Local, BoundedStaleness };
struct ReadOptions {
    ReadConsistency consistency = ReadConsistency::Strong;
    std::chrono::milliseconds lag_max{0};
};
struct TopologyEndpoint { std::string addr, region; };
struct TopologyReplica {
    TopologyEndpoint endpoint;
    bool healthy = false;
    uint32_t lag_ms = 0;
    uint64_t last_applied_lsn = 0;
    bool rebootstrapping = false;
};
struct Topology {
    uint64_t epoch = 0;
    TopologyEndpoint primary;
    std::vector<TopologyReplica> replicas;
};
} // namespace reddb

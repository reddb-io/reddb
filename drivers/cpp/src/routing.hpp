#pragma once
#include "reddb/reddb.hpp"
#include <functional>

namespace reddb::routing {
constexpr uint8_t topology_kind = 0x32;
constexpr uint32_t topology_feature = 2;
std::optional<Topology> decode_topology(std::span<const uint8_t> bytes);
std::optional<Topology> decode_topology_json(const std::string& json);
std::string endpoint(const std::string& address, const std::string& protocol);
std::pair<std::string, bool> sql_policy(std::string_view sql);
std::unique_ptr<Conn> connect(
    std::string seed, std::unique_ptr<Conn> initial, std::string protocol, bool force_primary,
    std::function<std::unique_ptr<Conn>(const std::string&)> dial);
} // namespace reddb::routing

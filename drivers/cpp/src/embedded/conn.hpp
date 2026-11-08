#pragma once

#include "reddb/reddb.hpp"
#include "reddb/url.hpp"

#include <memory>

namespace reddb::embedded {

std::unique_ptr<Conn> connect(const ParsedUri& uri);

} // namespace reddb::embedded

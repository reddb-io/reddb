#pragma once

#include <array>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <optional>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <type_traits>
#include <utility>
#include <variant>
#include <vector>

namespace reddb {

class Value {
public:
    struct Null {};
    struct Bytes { std::vector<std::byte> data; };
    struct Vector { std::vector<float> data; };
    struct Json { std::string text; };
    struct Timestamp { int64_t epoch_seconds; };
    struct Uuid {
        std::array<std::byte, 16> bytes;
        std::string canonical;
    };

    using Storage = std::variant<Null, bool, int64_t, double, std::string,
                                 Bytes, Vector, Json, Timestamp, Uuid>;

    Value() : storage_(Null{}) {}
    Value(std::nullopt_t) : storage_(Null{}) {}
    Value(bool value) : storage_(value) {}

    template <typename T,
              typename = std::enable_if_t<std::is_integral_v<T> &&
                                          !std::is_same_v<std::remove_cv_t<T>, bool>>>
    Value(T value) : storage_(checked_int64(value)) {}

    Value(float value) : storage_(static_cast<double>(value)) {}
    Value(double value) : storage_(value) {}
    Value(const char* value) : storage_(std::string(value)) {}
    Value(std::string value) : storage_(std::move(value)) {}
    Value(std::string_view value) : storage_(std::string(value)) {}

    static Value bytes(std::span<const std::byte> value);
    static Value vector(std::span<const float> value);
    static Value json(std::string_view value);
    static Value timestamp_seconds(int64_t epoch_seconds);
    static Value timestamp(std::chrono::system_clock::time_point value);
    static Value uuid(std::string_view value);

    const Storage& storage() const noexcept { return storage_; }

private:
    explicit Value(Storage storage) : storage_(std::move(storage)) {}

    template <typename T>
    static int64_t checked_int64(T value) {
        if constexpr (std::is_unsigned_v<T>) {
            if (value > static_cast<T>(std::numeric_limits<int64_t>::max())) {
                throw std::out_of_range("reddb Value integer exceeds i64 max");
            }
        }
        return static_cast<int64_t>(value);
    }

    Storage storage_;
};

} // namespace reddb

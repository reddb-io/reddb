cmake_minimum_required(VERSION 3.16)
project(reddb LANGUAGES CXX VERSION 0.1.0)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release)
endif()

if(CMAKE_CXX_COMPILER_ID MATCHES "GNU|Clang")
    add_compile_options(-Wall -Wextra -Wpedantic -Wno-unused-parameter)
endif()

# ----------------------------------------------------------------------------
# Required: OpenSSL (TLS + SCRAM)
# ----------------------------------------------------------------------------
find_package(OpenSSL REQUIRED)

# ----------------------------------------------------------------------------
# Optional: zstd (frame compression). When absent, COMPRESSED frames cannot
# be encoded (we just never set the bit) and decoding throws.
# ----------------------------------------------------------------------------
find_package(PkgConfig QUIET)
set(REDDB_HAS_ZSTD 0)
if(PkgConfig_FOUND)
    pkg_check_modules(ZSTD QUIET libzstd)
    if(ZSTD_FOUND)
        set(REDDB_HAS_ZSTD 1)
    endif()
endif()
if(NOT REDDB_HAS_ZSTD)
    find_library(ZSTD_LIBRARY NAMES zstd)
    find_path(ZSTD_INCLUDE_DIR NAMES zstd.h)
    if(ZSTD_LIBRARY AND ZSTD_INCLUDE_DIR)
        set(REDDB_HAS_ZSTD 1)
        set(ZSTD_LIBRARIES ${ZSTD_LIBRARY})
        set(ZSTD_INCLUDE_DIRS ${ZSTD_INCLUDE_DIR})
    endif()
endif()

# ----------------------------------------------------------------------------
# Optional: libcurl (HTTP transport).
# ----------------------------------------------------------------------------
find_package(CURL QUIET)
set(REDDB_HAS_CURL 0)
if(CURL_FOUND)
    set(REDDB_HAS_CURL 1)
endif()

# ----------------------------------------------------------------------------
# Library
# ----------------------------------------------------------------------------
set(REDDB_SOURCES
    src/url.cpp
    src/errors.cpp
    src/reddb.cpp
    src/redwire/frame.cpp
    src/redwire/codec.cpp
    src/redwire/scram.cpp
    src/redwire/conn.cpp
)
if(REDDB_HAS_CURL)
    list(APPEND REDDB_SOURCES src/http/client.cpp)
endif()

add_library(reddb STATIC ${REDDB_SOURCES})
target_include_directories(reddb PUBLIC
    $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
)
target_link_libraries(reddb PUBLIC OpenSSL::SSL OpenSSL::Crypto)

if(REDDB_HAS_ZSTD)
    target_compile_definitions(reddb PUBLIC REDDB_HAS_ZSTD=1)
    target_include_directories(reddb PUBLIC ${ZSTD_INCLUDE_DIRS})
    target_link_libraries(reddb PUBLIC ${ZSTD_LIBRARIES})
else()
    target_compile_definitions(reddb PUBLIC REDDB_HAS_ZSTD=0)
endif()

if(REDDB_HAS_CURL)
    target_compile_definitions(reddb PUBLIC REDDB_HAS_CURL=1)
    target_link_libraries(reddb PUBLIC CURL::libcurl)
else()
    target_compile_definitions(reddb PUBLIC REDDB_HAS_CURL=0)
endif()

# ----------------------------------------------------------------------------
# Tests via GoogleTest (FetchContent)
# ----------------------------------------------------------------------------
enable_testing()
include(FetchContent)
FetchContent_Declare(
    googletest
    URL https://github.com/google/googletest/archive/refs/tags/v1.14.0.tar.gz
    URL_HASH SHA256=8ad598c73ad796e0d8280b082cebd82a630d73e73cd3c70057938a6501bba5d7
    DOWNLOAD_EXTRACT_TIMESTAMP TRUE
)
set(gtest_force_shared_crt ON CACHE BOOL "" FORCE)
set(BUILD_GMOCK OFF CACHE BOOL "" FORCE)
FetchContent_MakeAvailable(googletest)

function(reddb_add_test name)
    add_executable(${name} tests/${name}.cpp)
    target_link_libraries(${name} PRIVATE reddb GTest::gtest_main)
    add_test(NAME ${name} COMMAND ${name})
endfunction()

reddb_add_test(url_test)
reddb_add_test(scram_test)
reddb_add_test(frame_test)
reddb_add_test(redwire_conn_test)
reddb_add_test(smoke_test)

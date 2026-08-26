cmake_minimum_required(VERSION 3.16)
project(libzmq_target LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Release profile. ARENA_ISA is the instruction-set baseline every target in the
# arena compiles against, so the comparison is between implementations rather
# than between whatever each build system happened to default to.
#
# This was -march=native, which has two problems. It pins the binary to the
# machine that compiled it, so it cannot be built once and shipped as an image;
# and it applied only here, because the Rust targets never pass target-cpu, so
# the C++ wrapper alone got microarchitecture-specific codegen.
#
# x86-64-v3 (AVX2, Haswell and later) is satisfied by any plausible bench host.
# Override with -DARENA_ISA=native for a local build that will not be shipped.
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()
if(NOT ARENA_ISA)
  set(ARENA_ISA "x86-64-v3")
endif()
set(CMAKE_CXX_FLAGS_RELEASE "-O3 -flto -march=${ARENA_ISA}")
set(CMAKE_INTERPROCEDURAL_OPTIMIZATION_RELEASE ON)

add_executable(libzmq_target src/main.cpp)

# libzmq discovery. Prefer pkg-config; fall back to find_library.
find_package(PkgConfig QUIET)
if(PkgConfig_FOUND)
  pkg_check_modules(ZMQ libzmq)
endif()

if(ZMQ_FOUND)
  target_include_directories(libzmq_target PRIVATE ${ZMQ_INCLUDE_DIRS})
  target_link_libraries(libzmq_target PRIVATE ${ZMQ_LIBRARIES})
else()
  find_path(ZMQ_INCLUDE_DIR zmq.h)
  find_library(ZMQ_LIBRARY NAMES zmq libzmq)
  if(NOT ZMQ_INCLUDE_DIR OR NOT ZMQ_LIBRARY)
    message(FATAL_ERROR "libzmq not found. Install libzmq3-dev (Debian) or zeromq-devel.")
  endif()
  target_include_directories(libzmq_target PRIVATE ${ZMQ_INCLUDE_DIR})
  target_link_libraries(libzmq_target PRIVATE ${ZMQ_LIBRARY})
endif()

cmake_minimum_required(VERSION 3.16)
project(libzmq_target LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Hyper-optimized release profile per the arena spec. -march=native ties the
# binary to the grid's bare-metal microarchitecture; do not ship these
# artifacts, rebuild on the runner.
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()
set(CMAKE_CXX_FLAGS_RELEASE "-O3 -flto -march=native")
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

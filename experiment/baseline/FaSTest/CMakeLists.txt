cmake_minimum_required(VERSION 3.21)
project(Fastest)

message(STATUS "C Compiler: ${CMAKE_C_COMPILER}")
message(STATUS "C++ Compiler: ${CMAKE_CXX_COMPILER}")
set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} -std=c++20 -Ofast")
include_directories(lib)

find_package(Boost QUIET)
if (Boost_FOUND)
    include_directories(${Boost_INCLUDE_DIRS})
else()
    file(GLOB BOOST_MODULAR_INCLUDES
        "${CMAKE_CURRENT_SOURCE_DIR}/../gcare/boost/libs/*/include"
        "${CMAKE_CURRENT_SOURCE_DIR}/../gcare/boost/libs/*/preprocessed/include"
    )
    if (BOOST_MODULAR_INCLUDES)
        message(STATUS "Using vendored Boost headers from ../gcare/boost")
        include_directories(${BOOST_MODULAR_INCLUDES})
    else()
        message(FATAL_ERROR "Boost headers not found")
    endif()
endif()


add_executable(Fastest driver/subgraph-cardinality-estimation.cc)
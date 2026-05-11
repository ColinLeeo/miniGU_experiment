cmake_minimum_required(VERSION 3.10)

project(Gcare CXX)

set(CMAKE_EXPORT_COMPILE_COMMANDS ON)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

if (NOT CMAKE_BUILD_TYPE OR CMAKE_BUILD_TYPE STREQUAL "")
    set(CMAKE_BUILD_TYPE "Release" CACHE STRING "" FORCE)
endif()

find_package(OpenMP)

add_subdirectory(${PROJECT_SOURCE_DIR}/boost EXCLUDE_FROM_ALL)

add_executable(gcare_graph ./src/main.cc ./src/util.cc ./src/data_graph.cc ./src/query_graph.cc ./src/wander_join.cc ./src/cset.cc ./src/sumrdf.cc ./src/jsub.cc ./src/impr.cc)
set_target_properties(gcare_graph PROPERTIES LINKER_LANGUAGE CXX)
target_include_directories(gcare_graph PRIVATE ${PROJECT_SOURCE_DIR}/include)
target_link_libraries(gcare_graph OpenMP::OpenMP_CXX Boost::regex Boost::program_options)

add_executable(gcare_relation ./src/main.cc ./src/util.cc ./src/ndvector.cc ./src/data_relations.cc ./src/query_relations.cc ./src/correlated_sampling.cc ./src/bound_sketch.cc)
set_target_properties(gcare_relation PROPERTIES LINKER_LANGUAGE CXX)
target_include_directories(gcare_relation PRIVATE ${PROJECT_SOURCE_DIR}/include)
target_compile_definitions(gcare_relation PRIVATE -DRELATION)
target_link_libraries(gcare_relation OpenMP::OpenMP_CXX Boost::regex Boost::program_options)

set(SOURCES
    ConnectionFromClient.cpp
)

set(GENERATED_SOURCES
    CompositorClientEndpoint.h
    CompositorServerEndpoint.h
)

add_library(compositorservice STATIC ${SOURCES} ${GENERATED_SOURCES})
ladybird_generated_sources(compositorservice)

add_executable(Compositor main.cpp)

target_include_directories(compositorservice PRIVATE ${CMAKE_CURRENT_BINARY_DIR}/../..)
target_include_directories(compositorservice PRIVATE ${LADYBIRD_SOURCE_DIR}/Services/)

target_link_libraries(Compositor PRIVATE compositorservice LibCore LibMain)
target_link_libraries(compositorservice PUBLIC LibCore LibGfx LibIPC LibWeb)

if (WIN32)
    ladybird_windows_bin(Compositor CONSOLE)
endif()

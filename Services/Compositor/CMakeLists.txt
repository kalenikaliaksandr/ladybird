set(SOURCES
    ConnectionFromClient.cpp
)

set(GENERATED_SOURCES
    CompositorClientEndpoint.h
    CompositorServerEndpoint.h
    WebContentCompositorClientEndpoint.h
    WebContentCompositorServerEndpoint.h
)

compile_ipc(${CMAKE_CURRENT_SOURCE_DIR}/WebContentCompositorClient.ipc ${CMAKE_BINARY_DIR}/Services/Compositor/WebContentCompositorClientEndpoint.h)
compile_ipc(${CMAKE_CURRENT_SOURCE_DIR}/WebContentCompositorServer.ipc ${CMAKE_BINARY_DIR}/Services/Compositor/WebContentCompositorServerEndpoint.h)

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

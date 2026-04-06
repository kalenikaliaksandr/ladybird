set(CMAKE_AUTOMOC OFF)
set(CMAKE_AUTORCC OFF)
set(CMAKE_AUTOUIC OFF)

set(SOURCES
    ConnectionFromClient.cpp
)

if (ANDROID)
    add_library(gpuprocessservice SHARED
        ${LADYBIRD_SOURCE_DIR}/UI/Android/src/main/cpp/GPUProcessService.cpp
        ${LADYBIRD_SOURCE_DIR}/UI/Android/src/main/cpp/LadybirdServiceBaseJNI.cpp
        ${SOURCES}
    )
    target_link_libraries(gpuprocessservice PRIVATE LibWebView)
else()
    add_library(gpuprocessservice STATIC ${SOURCES})
endif()

add_executable(GPUProcess main.cpp)

target_include_directories(gpuprocessservice PRIVATE ${CMAKE_CURRENT_BINARY_DIR}/../..)
target_include_directories(gpuprocessservice PRIVATE ${LADYBIRD_SOURCE_DIR}/Services/)

target_link_libraries(GPUProcess PRIVATE gpuprocessservice LibCore LibGfx LibMain LibWeb)
target_link_libraries(gpuprocessservice PRIVATE LibCore LibGfx LibIPC LibGPUProcessClient LibMain LibWeb)

if (WIN32)
    lagom_windows_bin(GPUProcess CONSOLE)
endif()

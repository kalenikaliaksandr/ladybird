set(SOURCES
    Client.cpp
)

if (NOT SERENITYOS)
    compile_ipc(../../Services/GPUProcess/GPUProcessClient.ipc ../../Services/GPUProcess/GPUProcessClientEndpoint.h)
    compile_ipc(../../Services/GPUProcess/GPUProcessServer.ipc ../../Services/GPUProcess/GPUProcessServerEndpoint.h)
endif()

set(GENERATED_SOURCES
    ../../Services/GPUProcess/GPUProcessClientEndpoint.h
    ../../Services/GPUProcess/GPUProcessServerEndpoint.h
)

ladybird_lib(LibGPUProcessClient gpuprocessclient)
target_link_libraries(LibGPUProcessClient PRIVATE LibCore LibIPC LibGfx)

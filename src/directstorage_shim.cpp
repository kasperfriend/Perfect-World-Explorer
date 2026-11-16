// Thin, optional bridge to Microsoft's DirectStorage system-memory queue.
// SDK headers/runtime are supplied by the developer, not redistributed here.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <dstorage.h>

using DStorageGetFactoryFn = HRESULT(WINAPI*)(REFIID, void**);

namespace {
SRWLOCK g_lock = SRWLOCK_INIT;
HMODULE g_runtime = nullptr;
IDStorageFactory* g_factory = nullptr;
IDStorageQueue1* g_queue = nullptr;
HANDLE g_event = nullptr;
bool g_attempted = false;

bool initialize() {
    if (g_attempted) return g_factory && g_queue && g_event;
    g_attempted = true;

    // The package runtime is optional. Failure leaves the caller on the normal
    // ReadFile path; do not make it a hard startup dependency.
    g_runtime = LoadLibraryExW(
        L"dstorage.dll", nullptr,
        LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
    if (!g_runtime) return false;
    auto getFactory = reinterpret_cast<DStorageGetFactoryFn>(
        GetProcAddress(g_runtime, "DStorageGetFactory"));
    if (!getFactory || FAILED(getFactory(__uuidof(IDStorageFactory),
                                         reinterpret_cast<void**>(&g_factory)))) {
        g_factory = nullptr;
        return false;
    }

    DSTORAGE_QUEUE_DESC desc{};
    desc.SourceType = DSTORAGE_REQUEST_SOURCE_FILE;
    desc.Capacity = DSTORAGE_MIN_QUEUE_CAPACITY;
    desc.Priority = DSTORAGE_PRIORITY_NORMAL;
    desc.Name = "Perfect World Explorer CPU reads";
    // A null device selects the DirectStorage system-memory destination queue.
    if (FAILED(g_factory->CreateQueue(&desc, __uuidof(IDStorageQueue1),
                                      reinterpret_cast<void**>(&g_queue)))) {
        g_queue = nullptr;
        return false;
    }
    g_event = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    return g_event != nullptr;
}
} // namespace

extern "C" __declspec(dllexport) int __cdecl pwe_dstorage_read(
    const wchar_t* path, unsigned long long offset, unsigned int size, void* output) {
    if (!path || !output || size == 0) return 0;
    AcquireSRWLockExclusive(&g_lock);
    int result = 0;
    if (initialize()) {
        IDStorageFile* file = nullptr;
        if (SUCCEEDED(g_factory->OpenFile(path, __uuidof(IDStorageFile),
                                          reinterpret_cast<void**>(&file)))) {
            DSTORAGE_REQUEST request{};
            request.Options.SourceType = DSTORAGE_REQUEST_SOURCE_FILE;
            request.Options.DestinationType = DSTORAGE_REQUEST_DESTINATION_MEMORY;
            request.Options.CompressionFormat = DSTORAGE_COMPRESSION_FORMAT_NONE;
            request.Source.File.Source = file;
            request.Source.File.Offset = offset;
            request.Source.File.Size = size;
            request.Destination.Memory.Buffer = output;
            request.Destination.Memory.Size = size;
            request.UncompressedSize = size;

            g_queue->EnqueueRequest(&request);
            g_queue->EnqueueSetEvent(g_event);
            g_queue->Submit();
            if (WaitForSingleObject(g_event, INFINITE) == WAIT_OBJECT_0) {
                DSTORAGE_ERROR_RECORD errors{};
                g_queue->RetrieveErrorRecord(&errors);
                result = errors.FailureCount == 0 ? 1 : 0;
            }
            file->Release();
        }
    }
    ReleaseSRWLockExclusive(&g_lock);
    return result;
}

// Thin, optional bridge to Microsoft's DirectStorage system-memory queue.
// SDK headers/runtime are supplied by the developer, not redistributed here.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <dstorage.h>
#include <string>

using DStorageGetFactoryFn = HRESULT(WINAPI*)(REFIID, void**);

namespace {
SRWLOCK g_factory_lock = SRWLOCK_INIT;
HMODULE g_runtime = nullptr;
IDStorageFactory* g_factory = nullptr;
bool g_factory_attempted = false;
std::wstring g_sdk_runtime_directory;

bool initialize_factory() {
    if (g_factory_attempted) return g_factory != nullptr;
    g_factory_attempted = true;

    // Prefer the portable package's side-by-side runtime, then the pinned
    // repository SDK runtime directory supplied by build.rs, then a system
    // installation. Failure leaves the caller on normal file I/O.
    g_runtime = LoadLibraryExW(
        L"dstorage.dll", nullptr, LOAD_LIBRARY_SEARCH_APPLICATION_DIR);
    if (!g_runtime && !g_sdk_runtime_directory.empty()) {
        std::wstring runtime_path = g_sdk_runtime_directory;
        if (runtime_path.back() != L'\\' && runtime_path.back() != L'/') {
            runtime_path.push_back(L'\\');
        }
        runtime_path += L"dstorage.dll";
        g_runtime = LoadLibraryExW(
            runtime_path.c_str(), nullptr,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    }
    if (!g_runtime) {
        g_runtime = LoadLibraryExW(
            L"dstorage.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
    if (!g_runtime) return false;
    auto getFactory = reinterpret_cast<DStorageGetFactoryFn>(
        GetProcAddress(g_runtime, "DStorageGetFactory"));
    if (!getFactory || FAILED(getFactory(__uuidof(IDStorageFactory),
                                         reinterpret_cast<void**>(&g_factory)))) {
        g_factory = nullptr;
        return false;
    }
    return true;
}

IDStorageFactory* get_factory() {
    AcquireSRWLockExclusive(&g_factory_lock);
    const bool ready = initialize_factory();
    IDStorageFactory* factory = ready ? g_factory : nullptr;
    ReleaseSRWLockExclusive(&g_factory_lock);
    return factory;
}

// Each calling thread owns its own queue. This keeps request completion/error
// records isolated while allowing independent asset-loader threads to issue
// and wait for reads concurrently; a process-global queue lock must never be
// held across a storage wait.
struct ThreadQueue {
    IDStorageQueue1* queue = nullptr;
    HANDLE event = nullptr;

    bool initialize(IDStorageFactory* factory) {
        if (queue && event) return true;
        DSTORAGE_QUEUE_DESC desc{};
        desc.SourceType = DSTORAGE_REQUEST_SOURCE_FILE;
        desc.Capacity = DSTORAGE_MIN_QUEUE_CAPACITY;
        desc.Priority = DSTORAGE_PRIORITY_NORMAL;
        desc.Name = "Perfect World Explorer CPU reads";
        // A null device selects the DirectStorage system-memory destination queue.
        if (FAILED(factory->CreateQueue(&desc, __uuidof(IDStorageQueue1),
                                        reinterpret_cast<void**>(&queue)))) {
            queue = nullptr;
            return false;
        }
        event = CreateEventW(nullptr, FALSE, FALSE, nullptr);
        if (!event) {
            queue->Release();
            queue = nullptr;
            return false;
        }
        return true;
    }

    ~ThreadQueue() {
        if (queue) queue->Release();
        if (event) CloseHandle(event);
    }
};

thread_local ThreadQueue g_thread_queue;
} // namespace

struct PweDStorageRange {
    unsigned long long offset;
    unsigned int size;
    void* output;
};

extern "C" __declspec(dllexport) void __cdecl pwe_dstorage_set_runtime_dir(
    const wchar_t* directory) {
    AcquireSRWLockExclusive(&g_factory_lock);
    if (!g_factory_attempted && directory) g_sdk_runtime_directory = directory;
    ReleaseSRWLockExclusive(&g_factory_lock);
}

// Submit several independent ranges before waiting. The caller groups entries
// from one archive, so one file open and one queue submit cover the whole batch.
extern "C" __declspec(dllexport) int __cdecl pwe_dstorage_read_batch(
    const wchar_t* path, const PweDStorageRange* ranges, unsigned int count) {
    if (!path || !ranges || count == 0 || count > DSTORAGE_MIN_QUEUE_CAPACITY) return 0;

    IDStorageFactory* factory = get_factory();
    if (!factory || !g_thread_queue.initialize(factory)) return 0;

    IDStorageFile* file = nullptr;
    if (FAILED(factory->OpenFile(path, __uuidof(IDStorageFile),
                                 reinterpret_cast<void**>(&file)))) {
        return 0;
    }

    for (unsigned int i = 0; i < count; ++i) {
        if (!ranges[i].output || ranges[i].size == 0) {
            file->Release();
            return 0;
        }
        DSTORAGE_REQUEST request{};
        request.Options.SourceType = DSTORAGE_REQUEST_SOURCE_FILE;
        request.Options.DestinationType = DSTORAGE_REQUEST_DESTINATION_MEMORY;
        request.Options.CompressionFormat = DSTORAGE_COMPRESSION_FORMAT_NONE;
        request.Source.File.Source = file;
        request.Source.File.Offset = ranges[i].offset;
        request.Source.File.Size = ranges[i].size;
        request.Destination.Memory.Buffer = ranges[i].output;
        request.Destination.Memory.Size = ranges[i].size;
        request.UncompressedSize = ranges[i].size;
        g_thread_queue.queue->EnqueueRequest(&request);
    }
    g_thread_queue.queue->EnqueueSetEvent(g_thread_queue.event);
    g_thread_queue.queue->Submit();
    const bool completed = WaitForSingleObject(g_thread_queue.event, INFINITE) == WAIT_OBJECT_0;

    bool succeeded = false;
    if (completed) {
        DSTORAGE_ERROR_RECORD errors{};
        g_thread_queue.queue->RetrieveErrorRecord(&errors);
        succeeded = errors.FailureCount == 0;
    }
    file->Release();
    return succeeded ? 1 : 0;
}

extern "C" __declspec(dllexport) int __cdecl pwe_dstorage_read(
    const wchar_t* path, unsigned long long offset, unsigned int size, void* output) {
    const PweDStorageRange range{offset, size, output};
    return pwe_dstorage_read_batch(path, &range, 1);
}

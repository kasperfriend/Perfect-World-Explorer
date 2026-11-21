// Experimental component-level geometry archive. Not a shippable tree baker:
// leaf map coordinates are recorded for one fixed camera and contain no wind
// or material/transition state. See PWCOMP02 in the investigation notes.
// Build: zig c++ -target x86-windows-gnu -O2 -static -municode tools/speedtree/component-cache-probe.cpp -o component-cache-probe.exe
// Usage: component-cache-probe.exe <SpeedTreeRT.dll> <one SPT> <output> <tree-type-id>
#define wmain unused_baker_main
#include "bake_experimental.cpp"
#undef wmain
using Count = unsigned short(METHOD*)(void*);

int wmain(int argc, wchar_t** argv) {
    if (argc != 5) { fprintf(stderr, "expected DLL, SPT, output and type id\n"); return 2; }
    wchar_t* end = nullptr;
    unsigned long type = wcstoul(argv[4], &end, 10);
    if (!*argv[4] || *end || type > 65535) { fprintf(stderr, "invalid tree type\n"); return 2; }
    _wremove(argv[3]);
    FILE* f = _wfopen(argv[2], L"rb");
    if (!f) { fprintf(stderr, "tree type %lu: cannot open SPT\n", type); return 1; }
    if (fseek(f, 0, SEEK_END)) { fclose(f); return 1; }
    long n = ftell(f); rewind(f);
    if (n < 1 || n > 100000000) { fclose(f); return 1; }
    std::vector<unsigned char> bytes(static_cast<size_t>(n));
    if (fread(bytes.data(), 1, bytes.size(), f) != bytes.size()) { fclose(f); return 1; }
    fclose(f);
    Api a = open_api(argv[1]);
    auto branch = reinterpret_cast<Count>(symbol(a.dll, "?GetNumBranchLodLevels@CSpeedTreeRT@@QBEGXZ"));
    auto frond = reinterpret_cast<Count>(symbol(a.dll, "?GetNumFrondLodLevels@CSpeedTreeRT@@QBEGXZ"));
    auto leaf = reinterpret_cast<Count>(symbol(a.dll, "?GetNumLeafLodLevels@CSpeedTreeRT@@QBEGXZ"));
    void* tree = a.alloc(0x9c);
    if (!tree) { fprintf(stderr, "tree type %lu: allocation failed\n", type); return 1; }
    a.ctor(tree);
    bool ok = false;
    try {
        if (!a.load(tree, bytes.data(), static_cast<unsigned>(bytes.size()))) throw a.error();
        float size = 0, variation = 0;
        a.getsize(tree, &size, &variation);
        a.setbranch(tree, 0); a.setfrond(tree, 0); a.setleaf(tree, 0);
        a.windb(tree, 2); a.windf(tree, 2); a.windl(tree, 2); a.groups(tree, 1);
        a.setsize(tree, size * 0.3048f, 0.f);
        if (!a.compute(tree, nullptr, 1, true)) throw a.error();
        using Drop = void(*)(bool);
        reinterpret_cast<Drop>(symbol(a.dll, "?SetDropToBillboard@CSpeedTreeRT@@SAX_N@Z"))(false);
        a.rock(tree, false); a.reset(tree); a.clock(0.f); a.strength(tree, 0.f, -1.f, -1.f);
        float eye[] = {0.f, 0.f, 0.f}, dir[] = {1.f, 0.f, 0.f};
        a.lod(tree, 1.f); a.camera(eye, dir);
        Geometry refresh{}; a.geo(tree, &refresh, 7, -1, -1, -1);
        Textures textures{}; a.tex(tree, &textures);
        unsigned counts[] = {branch(tree), frond(tree), leaf(tree)};
        for (unsigned count : counts) if (count > 64) throw "unsupported component LOD count";
        Writer w; w.raw("PWCOMP02", 8); w.u(a.dll_hash);
        uint32_t hash = 2166136261u;
        for (unsigned char byte : bytes) hash = (hash ^ byte) * 16777619u;
        w.u(hash); w.u(static_cast<uint32_t>(type)); w.text(textures.branch); w.text(textures.composite);
        for (unsigned component = 0; component < 3; ++component) {
            w.u(counts[component]);
            for (unsigned level = 0; level < counts[component]; ++level) {
                Geometry g{};
                a.geo(tree, &g, 1u << component,
                      component == 0 ? static_cast<short>(level) : -1,
                      component == 1 ? static_cast<short>(level) : -1,
                      component == 2 ? static_cast<short>(level) : -1);
                int actual = component == 0 ? g.branches.lod : component == 1 ? g.fronds.lod : g.leaves[0].lod;
                if (actual != static_cast<int>(level)) throw "runtime returned unexpected component LOD";
                if (component == 0) { indexed(w, g.branches, "branch"); continue; }
                if (component == 1) { indexed(w, g.fronds, "frond"); continue; }
                const Leaf& l = g.leaves[0];
                if (l.count && (!l.centers || !l.normals || !l.clusters || !l.texcoords || !l.mapcoords))
                    throw "leaf material: missing per-card geometry";
                w.u(l.count);
                auto leaf_float = [&w](float value) {
                    if (!std::isfinite(value)) throw "leaf material: nonfinite card geometry";
                    w.f(value);
                };
                for (unsigned i = 0; i < l.count; ++i) {
                    if (!l.texcoords[i] || !l.mapcoords[i]) throw "leaf material: missing card coordinates";
                    for (int j = 0; j < 3; ++j) leaf_float(l.centers[i*3+j]);
                    for (int j = 0; j < 3; ++j) leaf_float(l.normals[i*3+j]);
                    w.u(l.clusters[i]);
                    for (int j = 0; j < 8; ++j) leaf_float(l.texcoords[i][j]);
                    for (int j = 0; j < 16; ++j) leaf_float(l.mapcoords[i][j]);
                }
            }
        }
        FILE* out = _wfopen(argv[3], L"wb");
        if (!out) throw "cannot create output";
        bool saved = fwrite(w.b.data(), 1, w.b.size(), out) == w.b.size();
        if (fclose(out) || !saved) { _wremove(argv[3]); throw "cannot write output"; }
        printf("type %lu: branch %u frond %u leaf %u bytes %zu\n", type, counts[0], counts[1], counts[2], w.b.size());
        ok = true;
    } catch (const std::string& e) {
        fprintf(stderr, "tree type %lu: %s\n", type, e.c_str());
    } catch (const char* e) {
        fprintf(stderr, "tree type %lu: %s\n", type, e ? e : "runtime error");
    }
    a.dtor(tree); a.free(tree); FreeLibrary(a.dll);
    return ok ? 0 : 1;
}

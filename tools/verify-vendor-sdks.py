#!/usr/bin/env python3
"""Verify the vendored upscaling SDKs against the code that loads them.

There is no Windows/DX12 machine (and no GPU) in the development environment,
so the XeSS / XeFG / XeLL / FSR / DLSS / DLSS-G integration cannot be *run*
here. What can be
checked mechanically is everything up to the driver call:

1. Presence, size and SHA-256 of every runtime DLL the viewer names, and that
   the repository's ignore rules and packaging script actually let those files
   travel with the build.
2. Every export the Rust code resolves with `GetProcAddress` exists in the
   real DLL's PE export table, and the DLL name passed to `load_sdk_dll`
   matches the file. A typo (or an SDK bump that renamed an export) fails here
   instead of at runtime, where it would only show up as "XeSS unavailable".
3. Every numeric constant the FFI passes to the SDK (init flags, quality
   settings, marker ids, UI modes, resource ids) equals the enumerator in the
   vendored Intel header, so the bindings cannot silently drift from the SDK.

Exit code 0 means "the DLLs are where the loader looks, and everything the
loader asks for is there". It does not mean the GPU path works.

Usage: python3 tools/verify-vendor-sdks.py [--repo-root DIR] [--verbose]
"""

from __future__ import annotations

import argparse
import hashlib
import re
import struct
import sys
from pathlib import Path

# --- the allowlist: what the viewer expects to find in <exe>\sdk -----------
#
# Keep in sync with XESS_NAMES / XESS_FG_NAMES / XELL_NAMES in src/main.rs and
# with the staging lists in tools/package-windows.ps1. `verify_allowlists`
# checks that automatically.
EXPECTED_DLLS = {
    # name: (vendor, license text that must sit beside it in sdk/)
    "libxess.dll": "Intel_XeSS-SDK-3.0.2-LICENSE.txt",
    "libxess_fg.dll": "Intel_XeSS-SDK-3.0.2-LICENSE.txt",
    "libxell.dll": "Intel_XeSS-SDK-3.0.2-LICENSE.txt",
    "amd_fidelityfx_loader_dx12.dll": "AMD_FidelityFX-SDK-2.4.0-LICENSE.md",
    "amd_fidelityfx_upscaler_dx12.dll": "AMD_FidelityFX-SDK-2.4.0-LICENSE.md",
    "amd_fidelityfx_framegeneration_dx12.dll": "AMD_FidelityFX-SDK-2.4.0-LICENSE.md",
    "nvngx_dlss.dll": "NVIDIA_DLSS-SDK-LICENSE.txt",
    "nvngx_dlssg.dll": "NVIDIA_DLSS-SDK-LICENSE.txt",
}

# DLLs the viewer names in `src/` but that are not vendored: they are installed
# by something else and must never be staged in `sdk/`.
RUNTIME_DLLS = {
    # NGX Core: shipped by the display driver (DriverStore `_nvngx.dll` since
    # driver 471.11, older copies `nvngx.dll`, including a legacy one in
    # %SystemRoot%\System32). `src/dlss/loader.rs` finds it at run time and
    # drives the vendored DLSS snippets through it; the vendored files stay the
    # only ones in `sdk/`.
    "nvngx.dll": "NVIDIA display driver (NGX Core)",
}

# Recorded from the Intel XeSS SDK 3.0.2 release (and the AMD FidelityFX SDK
# 2.4.0 binaries). A mismatch is not automatically a failure — it means the
# vendored binary changed and THIRD_PARTY_NOTICES.md must be updated — but it
# is reported loudly.
KNOWN_SHA256 = {
    "libxess.dll": "251659dd84a3e84de67c886a4186e01f3eca49b00641906fe38bb6b807e5d5b7",
    "libxess_fg.dll": "ec5e0c65e075570c6ede72618bb666d0be0c2e10b2ea9762c0fe8cb8e375ab27",
    "libxell.dll": "d2030dcd694fda8f2ec7e044b13e6db8f0b56d4ba9113a5efad334e3f3ded8c7",
    # NVIDIA/DLSS, lib/Windows_x86_64/rel/ (git blobs 23c4672e…, 71b8070c…).
    "nvngx_dlss.dll": "d00b5b0ab29c3ca101b70c3a5ecb3a01b7ef33766a3383a28a18623083e6123e",
    "nvngx_dlssg.dll": "8d59d49b36410dd6b62fef2503a5871d347415bd85d943fa0848563a95dd50c7",
}

# --- PE export table ------------------------------------------------------


class PeImage:
    """Just enough PE parsing to read the export directory of a DLL."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self.data = path.read_bytes()
        if self.data[:2] != b"MZ":
            raise ValueError(f"{path.name}: not a PE image (no MZ header)")
        pe_offset = struct.unpack_from("<I", self.data, 0x3C)[0]
        if self.data[pe_offset : pe_offset + 4] != b"PE\0\0":
            raise ValueError(f"{path.name}: not a PE image (no PE signature)")
        coff = pe_offset + 4
        self.machine, self.section_count = struct.unpack_from("<HH", self.data, coff)
        optional_size = struct.unpack_from("<H", self.data, coff + 16)[0]
        optional = coff + 20
        magic = struct.unpack_from("<H", self.data, optional)[0]
        if magic == 0x20B:
            self.kind = "PE32+"
            self.image_base = struct.unpack_from("<Q", self.data, optional + 24)[0]
            directories = optional + 112
        elif magic == 0x10B:
            self.kind = "PE32"
            self.image_base = struct.unpack_from("<I", self.data, optional + 28)[0]
            directories = optional + 96
        else:
            raise ValueError(f"{path.name}: unknown optional header magic 0x{magic:x}")
        export_rva, export_size = struct.unpack_from("<II", self.data, directories)
        self.sections = []
        section = optional + optional_size
        for index in range(self.section_count):
            base = section + index * 40
            virtual_size, virtual_address, raw_size, raw_pointer = struct.unpack_from(
                "<IIII", self.data, base + 8
            )
            self.sections.append((virtual_address, virtual_size, raw_pointer, raw_size))
        self.exports = self._read_exports(export_rva, export_size)

    def _rva_to_offset(self, rva: int) -> int:
        for virtual_address, virtual_size, raw_pointer, raw_size in self.sections:
            span = max(virtual_size, raw_size)
            if virtual_address <= rva < virtual_address + span:
                return raw_pointer + (rva - virtual_address)
        raise ValueError(f"{self.path.name}: RVA 0x{rva:x} is not mapped by any section")

    def _read_exports(self, export_rva: int, export_size: int) -> set[str]:
        if export_rva == 0 or export_size == 0:
            return set()
        directory = self._rva_to_offset(export_rva)
        name_count = struct.unpack_from("<I", self.data, directory + 24)[0]
        names_rva = struct.unpack_from("<I", self.data, directory + 32)[0]
        names_offset = self._rva_to_offset(names_rva)
        exports: set[str] = set()
        for index in range(name_count):
            entry = names_offset + index * 4
            name_rva = struct.unpack_from("<I", self.data, entry)[0]
            offset = self._rva_to_offset(name_rva)
            end = self.data.index(b"\0", offset)
            exports.add(self.data[offset:end].decode("ascii", "replace"))
        return exports

    def __str__(self) -> str:
        return f"{self.path.name} ({self.kind}, {len(self.exports)} exports)"


# --- Rust source scanning -------------------------------------------------

LOAD_CALL = re.compile(
    r"load_sdk_dll\(\s*(?P<dir>[^,]+?)\s*,\s*\"(?P<dll>[^\"]+)\"\s*,\s*(?P<exports>EXPORTS|&\[[^\]]*\])",
    re.S,
)
EXPORT_CONST = re.compile(
    r"const\s+EXPORTS\s*:\s*&\[&\[u8\]\]\s*=\s*&\[(?P<body>.*?)\];", re.S
)
BYTE_STRING = re.compile(r"b\"(?P<name>[^\"]+)\\0\"")


# The AMD path resolves its FFX entry points through a local closure instead of
# `load_sdk_dll`, so scan for that shape too:
#     candidates.push(Path::new(dir).join("amd_fidelityfx_loader_dx12.dll"));
#     ... let get = |name: &[u8]| ...; get(b"ffxCreateContext\\0")?
FFX_GET = re.compile(r'get\(b"(?P<name>[^"]+)\\0"\)')
DLL_JOIN = re.compile(r'join\("(?P<dll>[A-Za-z0-9_.]+\.dll)"\)')


def rust_sources(repo: Path) -> list[Path]:
    return sorted(p for p in (repo / "src").rglob("*.rs"))


def exports_used_by_source(repo: Path) -> dict[str, set[str]]:
    """Map DLL name -> the export names the Rust code resolves from it."""
    wants: dict[str, set[str]] = {}
    for path in rust_sources(repo):
        text = path.read_text(encoding="utf-8", errors="replace")
        constants = {
            match.group("body")
            for match in EXPORT_CONST.finditer(text)
        }
        for match in LOAD_CALL.finditer(text):
            dll = match.group("dll")
            group = match.group("exports")
            body = group
            if group == "EXPORTS":
                # `EXPORTS` is a const in the same file; use the first block,
                # which is the one immediately above the call in every module
                # here (one SDK DLL per file).
                body = next(iter(constants), "")
            names = set(BYTE_STRING.findall(body))
            if names:
                wants.setdefault(dll, set()).update(names)
        # AMD-style local `get(...)` closure against the one DLL the file opens.
        joined = {match.group("dll") for match in DLL_JOIN.finditer(text)}
        if len(joined) == 1:
            gotten = set(FFX_GET.findall(text))
            if gotten:
                wants.setdefault(joined.pop(), set()).update(gotten)
    return wants


def dll_names_named_by_source(repo: Path) -> set[str]:
    names = set()
    for path in rust_sources(repo):
        text = path.read_text(encoding="utf-8", errors="replace")
        names.update(LOAD_CALL.findall(text) and [m.group("dll") for m in LOAD_CALL.finditer(text)])
    return names


# --- numeric constants ----------------------------------------------------

HEADER_CONSTANTS = {
    # rust file -> {rust const name: (header path, header enum/define name)}
    # The provider x quality table (`src/modes.rs`) owns every quality enum the
    # viewer hands to a vendor: NGX's `PerfQualityValue`, the FFX upscaler's
    # `qualityMode` and XeSS's `qualitySetting`. They are checked here because a
    # wrong value is not a compile error — it is a mode that silently renders at
    # another mode's ratio, or one the driver refuses at CreateFeature.
    "src/modes.rs": {
        "NGX_MAX_PERF": ("nvidia/nvsdk_ngx_defs.h", "NVSDK_NGX_PerfQuality_Value_MaxPerf"),
        "NGX_BALANCED": ("nvidia/nvsdk_ngx_defs.h", "NVSDK_NGX_PerfQuality_Value_Balanced"),
        "NGX_MAX_QUALITY": ("nvidia/nvsdk_ngx_defs.h", "NVSDK_NGX_PerfQuality_Value_MaxQuality"),
        "NGX_ULTRA_PERFORMANCE": (
            "nvidia/nvsdk_ngx_defs.h",
            "NVSDK_NGX_PerfQuality_Value_UltraPerformance",
        ),
        # `NVSDK_NGX_PerfQuality_Value_UltraQuality` (4) is deliberately absent
        # from the table: the installed core answers CreateFeature with
        # `0xbad00010` for it (docs/blockers.md B-28), so DLSS offers DLAA
        # instead. Its DLAA value is checked below.
        "NGX_DLAA": ("nvidia/nvsdk_ngx_defs.h", "NVSDK_NGX_PerfQuality_Value_DLAA"),
        "FFX_NATIVE_AA": ("upscalers/ffx_upscale.h", "FFX_UPSCALE_QUALITY_MODE_NATIVEAA"),
        "FFX_QUALITY": ("upscalers/ffx_upscale.h", "FFX_UPSCALE_QUALITY_MODE_QUALITY"),
        "FFX_BALANCED": ("upscalers/ffx_upscale.h", "FFX_UPSCALE_QUALITY_MODE_BALANCED"),
        "FFX_PERFORMANCE": ("upscalers/ffx_upscale.h", "FFX_UPSCALE_QUALITY_MODE_PERFORMANCE"),
        "FFX_ULTRA_PERFORMANCE": (
            "upscalers/ffx_upscale.h",
            "FFX_UPSCALE_QUALITY_MODE_ULTRA_PERFORMANCE",
        ),
        "XESS_ULTRA_PERFORMANCE": ("xess/xess.h", "XESS_QUALITY_SETTING_ULTRA_PERFORMANCE"),
        "XESS_PERFORMANCE": ("xess/xess.h", "XESS_QUALITY_SETTING_PERFORMANCE"),
        "XESS_BALANCED": ("xess/xess.h", "XESS_QUALITY_SETTING_BALANCED"),
        "XESS_QUALITY": ("xess/xess.h", "XESS_QUALITY_SETTING_QUALITY"),
        "XESS_ULTRA_QUALITY": ("xess/xess.h", "XESS_QUALITY_SETTING_ULTRA_QUALITY"),
        "XESS_AA": ("xess/xess.h", "XESS_QUALITY_SETTING_AA"),
    },
    "src/xess/imp.rs": {
        "XESS_INIT_FLAG_INVERTED_DEPTH": ("xess/xess.h", "XESS_INIT_FLAG_INVERTED_DEPTH"),
        "XESS_INIT_FLAG_LDR_INPUT_COLOR": ("xess/xess.h", "XESS_INIT_FLAG_LDR_INPUT_COLOR"),
        "XESS_LOGGING_LEVEL_INFO": ("xess/xess.h", "XESS_LOGGING_LEVEL_INFO"),
    },
    "src/xess/xell.rs": {
        "XELL_RESULT_SUCCESS": ("xell/xell.h", "XELL_RESULT_SUCCESS"),
        "XELL_SIMULATION_START": ("xell/xell.h", "XELL_SIMULATION_START"),
        "XELL_SIMULATION_END": ("xell/xell.h", "XELL_SIMULATION_END"),
        "XELL_RENDERSUBMIT_START": ("xell/xell.h", "XELL_RENDERSUBMIT_START"),
        "XELL_RENDERSUBMIT_END": ("xell/xell.h", "XELL_RENDERSUBMIT_END"),
        "XELL_PRESENT_START": ("xell/xell.h", "XELL_PRESENT_START"),
        "XELL_PRESENT_END": ("xell/xell.h", "XELL_PRESENT_END"),
        "XELL_LOGGING_LEVEL_WARNING": ("xell/xell.h", "XELL_LOGGING_LEVEL_WARNING"),
    },
    "src/xess/fg.rs": {
        "XEFG_INIT_FLAG_INVERTED_DEPTH": ("xess_fg/xefg_swapchain.h", "XEFG_SWAPCHAIN_INIT_FLAG_INVERTED_DEPTH"),
        "XEFG_UI_MODE_HUDLESS_UITEXTURE": ("xess_fg/xefg_swapchain.h", "XEFG_SWAPCHAIN_UI_MODE_HUDLESS_UITEXTURE"),
        "XEFG_SWAPCHAIN_RV_UNTIL_NEXT_PRESENT": ("xess_fg/xefg_swapchain.h", "XEFG_SWAPCHAIN_RV_UNTIL_NEXT_PRESENT"),
        "XEFG_SWAPCHAIN_RV_ONLY_NOW": ("xess_fg/xefg_swapchain.h", "XEFG_SWAPCHAIN_RV_ONLY_NOW"),
        "XEFG_SWAPCHAIN_RES_DEPTH": ("xess_fg/xefg_swapchain.h", "XEFG_SWAPCHAIN_RES_DEPTH"),
        "XEFG_SWAPCHAIN_RES_MOTION_VECTOR": ("xess_fg/xefg_swapchain.h", "XEFG_SWAPCHAIN_RES_MOTION_VECTOR"),
        "XEFG_SWAPCHAIN_RES_UI": ("xess_fg/xefg_swapchain.h", "XEFG_SWAPCHAIN_RES_UI"),
        "XEFG_LOGGING_LEVEL_INFO": ("xess_fg/xefg_swapchain.h", "XEFG_SWAPCHAIN_LOGGING_LEVEL_INFO"),
    },
    "src/dlss/imp.rs": {
        # The `PerfQualityValue` constants moved to `src/modes.rs`; what is left
        # here is the create flag the DLSS path writes.
        "NGX_DLSS_FEATURE_DEPTH_INVERTED": (
            "nvidia/nvsdk_ngx_defs.h",
            "NVSDK_NGX_DLSS_Feature_Flags_DepthInverted",
        ),
    },
    "src/dlss/loader.rs": {
        "NGX_VERSION_API": ("nvidia/nvsdk_ngx_defs.h", "NVSDK_NGX_VERSION_API_MACRO"),
        "NGX_FEATURE_SUPER_SAMPLING": ("nvidia/nvsdk_ngx_defs.h", "NVSDK_NGX_Feature_SuperSampling"),
        "NGX_FEATURE_FRAME_GENERATION": (
            "nvidia/nvsdk_ngx_defs.h",
            "NVSDK_NGX_Feature_FrameGeneration",
        ),
    },
    "src/dlss/params.rs": {
        "NGX_SUCCESS": ("nvidia/nvsdk_ngx_defs.h", "NVSDK_NGX_Result_Success"),
        "NGX_FAIL": ("nvidia/nvsdk_ngx_defs.h", "NVSDK_NGX_Result_Fail"),
    },
}

INT_LITERAL = re.compile(r"^(?P<value>0x[0-9a-fA-F_]+|\d[\d_]*|\d\s*<<\s*\d+)$")


def parse_int(text: str) -> int | None:
    text = text.strip()
    shift = re.fullmatch(r"(\d+)\s*<<\s*(\d+)", text)
    if shift:
        return int(shift.group(1)) << int(shift.group(2))
    if not re.fullmatch(r"-?(0x[0-9a-fA-F_]+|\d[\d_]*)", text):
        return None
    return int(text.replace("_", ""), 0)


def rust_const_value(text: str, name: str) -> int | None:
    match = re.search(
        rf"const\s+{re.escape(name)}\s*:\s*\w+\s*=\s*(?P<value>[^;]+);", text
    )
    if not match:
        return None
    return parse_int(match.group("value"))


ENUM_BLOCK = re.compile(r"enum\s+\w*\s*\{(?P<body>.*?)\}", re.S)
ENUM_MEMBER = re.compile(r"(?P<name>\w+)\s*(?:=\s*(?P<value>[^,\r\n]+))?")


def header_constant_value(text: str, name: str) -> int | None:
    """Resolve one enumerator (including implicitly numbered ones) or #define."""
    define = re.search(
        rf"^[ \t]*#[ \t]*define[ \t]+{re.escape(name)}[ \t]+(\S+)[ \t]*$", text, re.M
    )
    if define:
        value = parse_int(define.group(1))
        if value is not None:
            return value
    for block in ENUM_BLOCK.finditer(text):
        running = -1
        for member in ENUM_MEMBER.finditer(block.group("body")):
            member_name = member.group("name")
            if not member_name or member_name in ("typedef",):
                continue
            explicit = member.group("value")
            if explicit is None:
                # Implicitly numbered: previous value + 1.
                value = None if running is None else running + 1
            else:
                value = parse_int(explicit.strip())
            running = value
            if member_name == name:
                return value
    return None


def verify_constants(repo: Path, verbose: bool) -> list[str]:
    problems: list[str] = []
    checked = 0
    for rust_file, mapping in HEADER_CONSTANTS.items():
        rust_path = repo / rust_file
        if not rust_path.is_file():
            problems.append(f"{rust_file}: file the constant map names is missing")
            continue
        rust_text = rust_path.read_text(encoding="utf-8", errors="replace")
        for const_name, (header_file, header_name) in mapping.items():
            header_path = repo / "sdk/include" / header_file
            if not header_path.is_file():
                problems.append(f"{rust_file}: vendored header {header_file} is missing")
                continue
            header_text = header_path.read_text(encoding="utf-8", errors="replace")
            # Trailing `// required`-style comments would otherwise be read as
            # part of the enumerator value.
            header_text = re.sub(r"//[^\n]*", "", header_text)
            header_text = re.sub(r"/\*.*?\*/", "", header_text, flags=re.S)
            rust_value = rust_const_value(rust_text, const_name)
            header_value = header_constant_value(header_text, header_name)
            if rust_value is None:
                problems.append(f"{rust_file}: constant {const_name} not found")
                continue
            if header_value is None:
                problems.append(f"{header_file}: enumerator {header_name} not found")
                continue
            checked += 1
            if rust_value != header_value:
                problems.append(
                    f"{rust_file}: {const_name} = {rust_value} but "
                    f"{header_file}:{header_name} = {header_value}"
                )
            elif verbose:
                print(f"    ok  {const_name:<42} {rust_value:#x} ({header_file})")
    if verbose:
        print(f"  {checked} SDK constants match the vendored headers")
    return problems


# --- FFI struct layouts ---------------------------------------------------
#
# A wrong field order here does not fail to compile: it silently hands the SDK
# garbage. The check below compares the *kind sequence* of every struct the FFI
# fills in with the vendored header (pointer / 4-byte int / 4-byte float /
# double word / array / 2D pair), which catches reordering, omission and
# wrong-width fields. Signedness of 4-byte enums is not compared (C enums are
# `int`; the Rust side picks a 4-byte type for each).

STRUCT_PAIRS = {
    "src/xess/imp.rs": [
        ("Xess2d", "xess/xess.h", "_xess_2d_t"),
        ("XessInitParams", "xess/xess_d3d12.h", "_xess_d3d12_init_params_t"),
        ("XessExecuteParams", "xess/xess_d3d12.h", "_xess_d3d12_execute_params_t"),
    ],
    "src/xess/fg.rs": [
        ("Xefg2d", "xess_fg/xefg_swapchain.h", "_xefg_swapchain_2d_t"),
        (
            "XefgFrameConstantData",
            "xess_fg/xefg_swapchain.h",
            "_xefg_swapchain_frame_constant_data_t",
        ),
        (
            "XefgD3D12InitParams",
            "xess_fg/xefg_swapchain_d3d12.h",
            "_xefg_swapchain_d3d12_init_params_t",
        ),
        (
            "XefgD3D12ResourceData",
            "xess_fg/xefg_swapchain_d3d12.h",
            "_xefg_swapchain_d3d12_resource_data_t",
        ),
        ("XefgProperties", "xess_fg/xefg_swapchain.h", "_xefg_swapchain_properties_t"),
        ("XefgPresentStatus", "xess_fg/xefg_swapchain.h", "_xefg_swapchain_present_status_t"),
    ],
    "src/xess/xell.rs": [
        ("XellVersion", "xell/xell.h", "_xell_version_t"),
        ("XellSleepParams", "xell/xell.h", "_xell_sleep_params_t"),
    ],
}

RUST_STRUCT = re.compile(r"struct\s+(?P<name>\w+)\s*\{(?P<body>[^}]*)\}", re.S)
RUST_FIELD = re.compile(r"^\s*(?:pub\s+)?(?P<name>\w+)\s*:\s*(?P<type>[^,]+),", re.M)

C_ENUM_ALIASES = ("xess_quality_settings_t", "xefg_swapchain_ui_mode_t")
C_2D_ALIASES = ("xess_coord_t", "xess_2d_t", "xefg_swapchain_2d_t")


def rust_kind(type_text: str) -> str | None:
    text = type_text.strip()
    if text.startswith("*"):
        return "pointer"
    array = re.fullmatch(r"\[(\w+);\s*(\d+)\]", text)
    if array:
        inner = rust_kind(array.group(1))
        return f"{inner}x{array.group(2)}"
    if text in ("Xess2d", "Xefg2d"):
        return "u32x2"
    return {
        "f32": "f32",
        "u32": "int32",
        "i32": "int32",
        "u64": "u64",
        "u16": "u16",
        "u8": "u8",
        "bool": "u8",
    }.get(text)


def c_kind(decl: str) -> str | None:
    """Map one C member declaration to the same vocabulary as rust_kind."""
    text = " ".join(decl.split())
    if "*" in text:
        return "pointer"
    array = re.search(r"\[(\d+)\]", text)
    body = text.split(":")[0]
    words = body.replace(";", " ").split()
    if not words:
        return None
    type_words = [w for w in words if not re.fullmatch(r"[A-Za-z_]\w*(\[\d+\])?", w) or w in C_ENUM_ALIASES or w in C_2D_ALIASES or w.endswith("_t") or w in ("float", "uint32_t", "uint64_t", "int32_t", "int", "bool", "D3D12_RESOURCE_STATES", "D3D12_RESOURCE_FLAGS")]
    base = type_words[0] if type_words else words[0]
    # Order matters: `uint64_t`/`uint16_t` also end in `_t`, so the fixed-width
    # types are matched before the "any other SDK enum" fallback.
    if base in C_2D_ALIASES:
        kind = "u32x2"
    elif base == "float":
        kind = "f32"
    elif base == "uint64_t":
        kind = "u64"
    elif base == "uint16_t":
        kind = "u16"
    elif base in ("uint32_t", "D3D12_RESOURCE_STATES", "D3D12_RESOURCE_FLAGS"):
        kind = "int32"
    elif base in ("int32_t", "int") or base.endswith("_t"):
        kind = "int32"
    elif base == "bool":
        kind = "u8"
    else:
        return None
    if array:
        return f"{kind}x{array.group(1)}"
    return kind


# Fields whose Rust name deliberately differs from the header's.
DELIBERATE_RENAMES = {
    ("XefgD3D12ResourceData", "resource_type"): "type",
    # `xell_sleep_params_t` packs its flags into bitfields; Rust keeps one word.
    ("XellSleepParams", "flags"): "bLowLatencyMode",
}


def normalise_field(name: str) -> str:
    """`outputColorBase` and `output_color_base` normalise to the same word."""
    return name.strip().lstrip("_").lower().replace("_", "")


def field_matches(rust_name: str, c_name: str) -> bool:
    """Rust drops the SDK's `p` pointer prefix; nothing else may differ."""
    rust = normalise_field(rust_name)
    c = normalise_field(c_name)
    return rust == c or (c.startswith("p") and rust == c[1:])


def member_name(decl: str) -> str | None:
    text = " ".join(decl.split())
    text = text.split(":")[0]
    match = re.search(r"([A-Za-z_]\w*)\s*(?:\[[^\]]*\])?\s*$", text.strip().rstrip(";"))
    return match.group(1) if match else None


def c_member_names(header_text: str, struct_tag: str) -> list[str] | None:
    match = re.search(
        rf"typedef\s+struct\s+{re.escape(struct_tag)}\s*\{{(?P<body>.*?)\}}\s*\w+;",
        header_text,
        re.S,
    )
    if not match:
        return None
    body = re.sub(r"/\*.*?\*/", "", match.group("body"), flags=re.S)
    body = re.sub(r"//[^\n]*", "", body)
    names: list[str] = []
    previous_bitfield = None
    for decl in body.split(";"):
        decl = decl.strip()
        if not decl:
            continue
        base_kind = c_kind(decl)
        if base_kind is None:
            return None
        bitfield = ":" in decl
        name = member_name(decl)
        if name is None:
            return None
        if bitfield:
            if previous_bitfield == base_kind:
                continue
            previous_bitfield = base_kind
        else:
            previous_bitfield = None
        names.append(name)
    return names


def c_members(header_text: str, struct_tag: str) -> list[str] | None:
    match = re.search(
        rf"typedef\s+struct\s+{re.escape(struct_tag)}\s*\{{(?P<body>.*?)\}}\s*\w+;",
        header_text,
        re.S,
    )
    if not match:
        return None
    body = re.sub(r"/\*.*?\*/", "", match.group("body"), flags=re.S)
    body = re.sub(r"//[^\n]*", "", body)
    kinds: list[str] = []
    previous_bitfield: str | None = None
    for decl in body.split(";"):
        decl = decl.strip()
        if not decl:
            continue
        bitfield = ":" in decl
        kind = c_kind(decl)
        if kind is None:
            return None
        if bitfield:
            # Consecutive bitfields of one base type share a storage unit.
            if previous_bitfield == kind:
                continue
            previous_bitfield = kind
        else:
            previous_bitfield = None
        kinds.append(kind)
    return kinds


def verify_struct_layouts(repo: Path, verbose: bool) -> list[str]:
    problems: list[str] = []
    checked = 0
    for rust_file, pairs in STRUCT_PAIRS.items():
        rust_text = (repo / rust_file).read_text(encoding="utf-8", errors="replace")
        structs = {
            match.group("name"): match.group("body")
            for match in RUST_STRUCT.finditer(rust_text)
        }
        for rust_name, header_file, header_tag in pairs:
            body = structs.get(rust_name)
            if body is None:
                problems.append(f"{rust_file}: struct {rust_name} is missing")
                continue
            header_text = (repo / "sdk/include" / header_file).read_text(
                encoding="utf-8", errors="replace"
            )
            rust_fields = list(RUST_FIELD.finditer(body))
            rust_kinds = [rust_kind(m.group("type")) for m in rust_fields]
            rust_names = [m.group("name") for m in rust_fields]
            header_kinds = c_members(header_text, header_tag)
            header_names = c_member_names(header_text, header_tag)
            if any(kind is None for kind in rust_kinds):
                problems.append(
                    f"{rust_file}: {rust_name} has a field type this check does not understand"
                )
                continue
            if header_kinds is None:
                problems.append(f"{header_file}: cannot read {header_tag}")
                continue
            if header_names is None:
                problems.append(f"{header_file}: cannot read {header_tag} member names")
                continue
            checked += 1
            if rust_kinds != header_kinds:
                problems.append(
                    f"{rust_file}: {rust_name} layout {rust_kinds} != "
                    f"{header_file}:{header_tag} layout {header_kinds}"
                )
                continue
            # Same-width fields can be swapped without changing the layout; the
            # names are what catches a jitter X/Y swap or a depth/MV mix-up.
            mismatched: list[str] = []
            for rust_field, header_field in zip(rust_names, header_names):
                if (rust_name, rust_field) in DELIBERATE_RENAMES:
                    # Reviewed rename; the table records both spellings.
                    assert DELIBERATE_RENAMES[(rust_name, rust_field)] == header_field
                    continue
                if not field_matches(rust_field, header_field):
                    mismatched.append(f"{rust_field} != {header_field}")
            if mismatched:
                problems.append(
                    f"{rust_file}: {rust_name} field names do not line up with "
                    f"{header_file}:{header_tag}: " + ", ".join(mismatched)
                )
            elif verbose:
                print(f"    ok  {rust_name:<24} {len(rust_kinds)} fields ({header_tag})")
    if verbose:
        print(f"  {checked} FFI struct layouts match their headers")
    return problems


# --- repository wiring ----------------------------------------------------


def verify_allowlists(repo: Path) -> list[str]:
    """The probe, the loader and the packaging script must agree."""
    problems: list[str] = []
    main_rs = (repo / "src/main.rs").read_text(encoding="utf-8")
    for dll in EXPECTED_DLLS:
        # The probe gates the Graphics-tab options on these name arrays.
        if f'"{dll}"' not in main_rs:
            problems.append(
                f"src/main.rs: probe_sdk never looks for {dll}, so the option "
                f"stays disabled even with the file in place"
            )
    package = (repo / "tools/package-windows.ps1")
    if package.is_file():
        package_text = package.read_text(encoding="utf-8")
        for dll in EXPECTED_DLLS:
            if dll not in package_text:
                problems.append(
                    f"tools/package-windows.ps1: {dll} is not staged, so the "
                    f"packaged viewer will not find it"
                )
    else:
        problems.append("tools/package-windows.ps1 is missing")
    ignore = (repo / ".gitignore")
    if ignore.is_file():
        ignore_text = ignore.read_text(encoding="utf-8")
        for dll in EXPECTED_DLLS:
            if f"!{dll}" in ignore_text or f"!/sdk/{dll}" in ignore_text:
                continue
            problems.append(
                f".gitignore: {dll} is not un-ignored, so it cannot be committed"
            )
    else:
        problems.append(".gitignore is missing")
    # The loader and the probe must search the same directories.
    probe = re.search(r"pub fn probe_sdk.*?\n\}", main_rs, re.S)
    loader = (repo / "src/xess/loader.rs").read_text(encoding="utf-8")
    if probe:
        body = probe.group(0)
        if "PW_UPSCALER_SDK_DIR" not in body or "PW_UPSCALER_SDK_DIR" not in loader:
            problems.append(
                "src/main.rs and src/xess/loader.rs disagree about PW_UPSCALER_SDK_DIR"
            )
        if 'join("sdk")' not in body or 'join("sdk")' not in loader:
            problems.append(
                "src/main.rs and src/xess/loader.rs disagree about the <exe>\\sdk folder"
            )
    else:
        problems.append("src/main.rs: probe_sdk not found")
    return problems


# --- main -----------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo-root",
        default=str(Path(__file__).resolve().parent.parent),
        help="repository root (default: the parent of tools/)",
    )
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()
    repo = Path(args.repo_root).resolve()
    sdk = repo / "sdk"

    problems: list[str] = []
    print(f"repository: {repo}")
    print(f"sdk folder: {sdk}\n")

    # 1. Files, sizes, hashes, licenses.
    print("1. vendored runtime binaries")
    images: dict[str, PeImage] = {}
    for dll, license_name in EXPECTED_DLLS.items():
        path = sdk / dll
        if not path.is_file():
            problems.append(f"sdk/{dll} is missing")
            print(f"  MISSING {dll}")
            continue
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        size = path.stat().st_size
        try:
            image = PeImage(path)
            images[dll] = image
            shape = f"{image.kind}, {len(image.exports)} exports"
        except ValueError as error:
            problems.append(str(error))
            shape = "unreadable"
        note = ""
        expected = KNOWN_SHA256.get(dll)
        if expected and expected != digest:
            note = "  (hash differs from the recorded release: update THIRD_PARTY_NOTICES.md)"
        print(f"  {dll:<42} {size:>10,} B  sha256 {digest[:16]}…  {shape}{note}")
        if not (sdk / license_name).is_file():
            problems.append(
                f"sdk/{license_name} is missing; the license requires the notice "
                f"to travel with the redistributed binary"
            )
        elif args.verbose:
            print(f"      license: sdk/{license_name}")
    for dll, expected in KNOWN_SHA256.items():
        if dll not in EXPECTED_DLLS:
            continue
        path = sdk / dll
        if path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            print(f"  note: {dll} does not match the recorded SDK 3.0.2 hash")

    # 2. Everything the loader resolves must exist in the DLL.
    print("\n2. GetProcAddress names vs. the DLL export tables")
    wants = exports_used_by_source(repo)
    if not wants:
        problems.append(
            "no load_sdk_dll call with an export list was found in src/ — the "
            "scanner is out of date with the integration and proves nothing"
        )
    for dll, names in sorted(wants.items()):
        image = images.get(dll)
        if image is None and dll in RUNTIME_DLLS:
            # Supplied by the driver, not vendored: only the *name* is checked.
            print(
                f"  {dll}: runtime DLL from the {RUNTIME_DLLS[dll]}, "
                f"{len(names)} export(s) resolved dynamically"
            )
            continue
        if image is None:
            problems.append(f"{dll}: named by src/ but not in sdk/")
            print(f"  {dll}: MISSING, {len(names)} export(s) cannot resolve")
            continue
        missing = sorted(names - image.exports)
        print(f"  {dll}: {len(names)} export(s) requested, {len(names) - len(missing)} found")
        if missing:
            for name in missing:
                problems.append(f"{dll}: export {name} is not exported by the DLL")
                print(f"      MISSING EXPORT {name}")
        elif args.verbose:
            for name in sorted(names):
                print(f"      ok  {name}")

    # 3. Constants vs. the vendored headers.
    print("\n3. FFI constants vs. the vendored Intel headers")
    problems.extend(verify_constants(repo, args.verbose))

    # 3b. Struct layouts the FFI fills in.
    print("\n3b. FFI struct layouts vs. the vendored Intel headers")
    problems.extend(verify_struct_layouts(repo, args.verbose))

    # 4. The probe / loader / packaging / ignore rules agree.
    print("\n4. viewer wiring (probe, loader, packaging, ignore rules)")
    wiring = verify_allowlists(repo)
    problems.extend(wiring)
    if not wiring:
        print("  probe, loader, packaging script and .gitignore all name the same files")

    print()
    if problems:
        print(f"FAILED with {len(problems)} problem(s):")
        for problem in problems:
            print(f"  - {problem}")
        return 1
    print(
        "OK: every DLL the viewer loads is vendored, every export it resolves is "
        "present, the FFI constants match the headers, and the packaged layout "
        "finds them."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

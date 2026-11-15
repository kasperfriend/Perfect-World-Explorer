//! Whole-archive BMD census for Phase 3 Agent A (P3.01/P3.03 evidence).
//!
//! Scans EVERY member of litmodels.pck and building.pck, classifies it by the
//! magic bytes the retail client sniffs (AFileImage::Open 0x76da20: "MOXB"
//! binary / "MOXT" text / plain), walks BMD model+mesh headers exactly like
//! A3DLitModel::Load (0x7bb380) / A3DLitMesh::Load (0x7ba070), and records
//! every special case WITH the archive member it came from.
//!
//! usage: bmd-corpus-census <client-root> [output.md]
#![allow(dead_code)]
#[path = "../bmd.rs"]
mod bmd;
#[path = "../pck.rs"]
mod pck;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

fn u32le(b: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(p..p + 4)?.try_into().ok()?))
}

/// Same normalization the client's package lookup applies: GBK decode,
/// lowercase, backslashes to slashes (AFilePackage lowercases before the
/// directory search; 0x770e70 rewrites separators).
fn norm(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    let (t, _, _) = encoding_rs::GBK.decode(&bytes[..end]);
    let t = t.replace('\\', "/").to_lowercase();
    t.trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned()
}

fn norm_name(s: &str) -> String {
    let t = s.replace('\\', "/").to_lowercase();
    t.trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned()
}

struct Case {
    archive: String,
    member: String,
    kind: String,
    detail: String,
}

#[derive(Default)]
struct WalkStats {
    files: usize,
    wrapper: usize,
    wrapper_byte8: BTreeMap<u8, usize>,
    model_versions: BTreeMap<u32, usize>,
    mesh_versions: BTreeMap<u32, usize>,
    meshes: usize,
    vertices: u64,
    faces: u64,
    two_sided: usize,
    with_material: usize,
    material_names: BTreeMap<String, usize>,
    truncated_meshes: usize,
    stopped_scan: usize,
    trailer_files: usize,
    trailer_groups: usize,
    trailer_ids: usize,
    trailer_bad_ids: usize,
    trailer_max_id: u32,
    trailer_lights: usize,
    trailer_flag_bytes: BTreeMap<u8, usize>,
}

#[derive(Default)]
struct ArchiveStats {
    name: String,
    members: usize,
    bytes: u64,
    classes: BTreeMap<String, usize>,
    class_bytes: BTreeMap<String, u64>,
    exts: BTreeMap<String, usize>,
    read_errors: usize,
    bmd: WalkStats,
    parse_results: BTreeMap<String, usize>,
    non_bmd_textures: HashSet<String>,
}

fn classify(data: &[u8], name: &str) -> String {
    if data.starts_with(&0x42584f4du32.to_le_bytes()) {
        return "bmd-moxb".into();
    }
    if data.starts_with(b"MOXT") {
        return "moxt-text".into();
    }
    if data.starts_with(b"DDS ") {
        return "dds".into();
    }
    if data.starts_with(b"BM") {
        return "bmp".into();
    }
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        return "png".into();
    }
    if data.starts_with(b"\xff\xd8\xff") {
        return "jpg".into();
    }
    let lower = name.to_lowercase();
    if lower.ends_with(".tga") {
        return "tga-by-extension".into();
    }
    if lower.ends_with(".bmd") {
        return "bmd-extension-other-magic".into();
    }
    "other".into()
}

fn ext_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((_, e)) if !e.contains('/') => format!(".{}", e.to_lowercase()),
        _ => "<none>".into(),
    }
}

/// Walk one BMD member's model/mesh headers. Returns texture references and
/// nrf-derived second-texture references (A3DLitMesh::LoadTexture 0x7b9880).
fn walk_bmd(
    data: &[u8],
    st: &mut WalkStats,
    cases: &mut Vec<Case>,
    arch: &str,
    member: &str,
    tex_refs: &mut Vec<String>,
    nrf_refs: &mut Vec<(String, String)>,
) {
    st.files += 1;
    let case = |cases: &mut Vec<Case>, kind: &str, detail: String| {
        cases.push(Case {
            archive: arch.to_owned(),
            member: member.to_owned(),
            kind: kind.to_owned(),
            detail,
        })
    };
    let mut p = 4usize;
    let Some(mut version) = u32le(data, p) else {
        case(cases, "bmd-header-truncated", "no version dword".into());
        return;
    };
    if version == 0x80000001 {
        st.wrapper += 1;
        match data.get(8) {
            Some(b) => *st.wrapper_byte8.entry(*b).or_insert(0) += 1,
            None => {
                case(
                    cases,
                    "bmd-wrapper-truncated",
                    "no byte after chunk tag".into(),
                );
                return;
            }
        }
        p = 9;
        match u32le(data, p) {
            Some(v) => version = v,
            None => {
                case(
                    cases,
                    "bmd-header-truncated",
                    "no version after wrapper".into(),
                );
                return;
            }
        }
        p += 4;
    } else {
        p += 4;
    }
    *st.model_versions.entry(version).or_insert(0) += 1;
    // A3DLitModel::Load accepts 0x10000001 (0x7bb3de) and 0x10000002
    // (0x7bb58b) with identical layout; everything else logs "Wrong version!".
    if version != 0x10000001 && version != 0x10000002 {
        case(
            cases,
            "model-wrong-version",
            format!("model version {version:#010x}"),
        );
        return;
    }
    // scale/direction/up/position = 48 bytes, then mesh count.
    p += 48;
    let Some(count) = u32le(data, p) else {
        case(cases, "bmd-truncated", "no mesh count".into());
        return;
    };
    p += 4;
    if count > 100_000 {
        case(cases, "implausible-mesh-count", format!("count {count}"));
        return;
    }
    for mesh_idx in 0..count as usize {
        let Some(mver) = u32le(data, p) else {
            st.truncated_meshes += 1;
            case(
                cases,
                "mesh-truncated",
                format!("mesh {mesh_idx}: no version dword"),
            );
            return;
        };
        p += 4;
        *st.mesh_versions.entry(mver).or_insert(0) += 1;
        if data.len() < p + 328 {
            st.truncated_meshes += 1;
            case(
                cases,
                "mesh-truncated",
                format!("mesh {mesh_idx}: header cut (version {mver:#010x})"),
            );
            return;
        }
        let tex = norm(&data[p + 64..p + 320]);
        let n = u32le(data, p + 320).unwrap_or(u32::MAX) as u64;
        let faces = u32le(data, p + 324).unwrap_or(u32::MAX) as u64;
        p += 328;
        if n > 10_000_000 || faces > 20_000_000 {
            st.stopped_scan += 1;
            case(
                cases,
                "implausible-mesh-size",
                format!("mesh {mesh_idx}: verts {n} faces {faces} (version {mver:#010x})"),
            );
            return;
        }
        st.meshes += 1;
        st.vertices += n;
        st.faces += faces;
        // A3DLitMesh::Load: v1 -> silent false (0x7baa64); v2 0x7ba0b8;
        // v3 0x7ba30d; v4 0x7ba573; v5 0x7ba7a7; else "Unknown version [%x]".
        let (advance, has_material) = match mver {
            0x10000004 => (n * 24 + faces * 6 + n * 12 + n * 4 + n * 4 + 48, false),
            0x10000005 => (n * 24 + faces * 6 + n * 12 + n * 4 + n * 4 + 48, true),
            0x10000002 => (n * 28 + faces * 6 + n * 12 + 48, false),
            0x10000003 => (n * 28 + faces * 6 + n * 12 + n * 4 + n * 4 + 48, false),
            0x10000001 => {
                st.stopped_scan += 1;
                case(
                    cases,
                    "mesh-v1-client-rejects",
                    format!(
                        "mesh {mesh_idx}: client A3DLitMesh::Load returns false without logging"
                    ),
                );
                return;
            }
            other => {
                st.stopped_scan += 1;
                case(
                    cases,
                    "mesh-unknown-version",
                    format!("mesh {mesh_idx}: version {other:#010x}"),
                );
                return;
            }
        };
        if p.checked_add(advance as usize)
            .is_none_or(|e| e > data.len())
        {
            st.truncated_meshes += 1;
            case(
                cases,
                "mesh-truncated",
                format!(
                    "mesh {mesh_idx}: needs {advance} more bytes, {} available (version {mver:#010x})",
                    data.len().saturating_sub(p)
                ),
            );
            return;
        }
        p += advance as usize;
        if has_material {
            // v5 trailing material (loader 0x7baa14 -> 0x7b67b0): NUL-terminated
            // name, 4x4 floats, power, two-sided byte.
            let tail = &data[p..];
            let Some(len) = tail.iter().position(|&c| c == 0) else {
                st.truncated_meshes += 1;
                case(
                    cases,
                    "material-truncated",
                    format!("mesh {mesh_idx}: unterminated material name"),
                );
                return;
            };
            if len > 4096 || tail.len() < len + 1 + 69 {
                st.truncated_meshes += 1;
                case(
                    cases,
                    "material-truncated",
                    format!("mesh {mesh_idx}: material tail len {len} of {}", tail.len()),
                );
                return;
            }
            let mname = norm(&tail[..len]);
            *st.material_names.entry(mname).or_insert(0) += 1;
            let base = p + len + 1;
            let two_sided = data.get(base + 68).copied().unwrap_or(0) != 0;
            st.with_material += 1;
            if two_sided {
                st.two_sided += 1;
            }
            p = base + 69;
        }
        if !tex.is_empty() {
            tex_refs.push(tex.clone());
            // A3DLitMesh::LoadTexture: when strlen(name) > 4 and the name does
            // not contain "nrf_", the client loads a second texture
            // shaders\textures\nrf\<basename> (0x7b9880..0x7b995e), falling
            // back to shaders\textures\nrf\rf_default.dds when that file does
            // not exist (0x76f1e0 existence check at 0x7b9927).
            let chars: Vec<char> = tex.chars().collect();
            if chars.len() > 4 && !tex.contains("nrf_") {
                let basename = tex.rsplit('/').next().unwrap_or(&tex).to_owned();
                nrf_refs.push((tex, format!("shaders/textures/nrf/{basename}")));
            }
        } else {
            tex_refs.push(String::new());
        }
    }
    // Post-model trailer, consumed by the scenery container, not by
    // A3DLitModel::Load: the loader at 0x64e200 reads a u32 group count into
    // container+0xc (0x64e307), then 0x64dd30 reads count x [u32 N][N u32
    // ids] and 0x64e010 reads count light-group records via 0x72a0a0:
    // [48 bytes][u32][u32 M][M x (12-byte direction + 4-byte float + 1 flag)].
    // These records drive scenery light attachments, not geometry/textures.
    let Some(count_a) = u32le(data, p) else {
        st.truncated_meshes += 1;
        case(cases, "trailer-truncated", "no group count".into());
        return;
    };
    p += 4;
    if count_a > 100_000 {
        case(
            cases,
            "trailer-implausible",
            format!("group count {count_a}"),
        );
        return;
    }
    st.trailer_files += 1;
    st.trailer_groups += count_a as usize;
    for g in 0..count_a as usize {
        let Some(n) = u32le(data, p) else {
            case(
                cases,
                "trailer-truncated",
                format!("group {g}: no id count"),
            );
            return;
        };
        p += 4;
        if n > 100_000 {
            case(
                cases,
                "trailer-implausible",
                format!("group {g}: id count {n}"),
            );
            return;
        }
        for i in 0..n as usize {
            let Some(id) = u32le(data, p) else {
                case(
                    cases,
                    "trailer-truncated",
                    format!("group {g}: cut at id {i}"),
                );
                return;
            };
            p += 4;
            if id >= count as u32 {
                st.trailer_bad_ids += 1;
            }
            st.trailer_max_id = st.trailer_max_id.max(id);
        }
        st.trailer_ids += n as usize;
    }
    for g in 0..count_a as usize {
        if data.len() < p + 56 {
            case(
                cases,
                "trailer-truncated",
                format!("light group {g}: header cut"),
            );
            return;
        }
        p += 56;
        let m = u32le(data, p - 4).unwrap_or(0) as usize;
        if m > 1_000_000 {
            case(
                cases,
                "trailer-implausible",
                format!("light group {g}: light count {m}"),
            );
            return;
        }
        for i in 0..m {
            match data.get(p + 16) {
                Some(flag) => *st.trailer_flag_bytes.entry(*flag).or_insert(0) += 1,
                None => {
                    case(
                        cases,
                        "trailer-truncated",
                        format!("light group {g}: light {i} cut"),
                    );
                    return;
                }
            }
            p += 17;
        }
        st.trailer_lights += m;
    }
    if p != data.len() {
        case(
            cases,
            "bmd-trailing-bytes",
            format!("{} bytes after decoded trailer", data.len() - p),
        );
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let a: Vec<_> = std::env::args().collect();
    if a.len() < 2 || a.len() > 3 {
        return Err("usage: bmd-corpus-census <client-root> [output.md]".into());
    }
    let root = Path::new(&a[1]);
    let element = if root.join("element").is_dir() {
        root.join("element")
    } else {
        root.to_path_buf()
    };
    let out = a
        .get(2)
        .cloned()
        .unwrap_or_else(|| "Findings/Phase-3-Agent-A-bmd-corpus-census.md".into());

    let mut archives: Vec<ArchiveStats> = Vec::new();
    let mut cases: Vec<Case> = Vec::new();
    let mut tex_refs: Vec<String> = Vec::new();
    let mut nrf_refs: Vec<(String, String)> = Vec::new();
    let mut all_members: HashMap<String, Vec<String>> = HashMap::new();

    for archive in ["litmodels.pck", "building.pck"] {
        let mut st = ArchiveStats {
            name: archive.to_owned(),
            ..Default::default()
        };
        let path = element.join(archive);
        if !path.exists() {
            return Err(format!("missing archive {}", path.display()));
        }
        let mut f = pck::PackageFile::open(&path).map_err(|e| e.to_string())?;
        let entries = pck::directory(&mut f)?;
        for e in &entries {
            let name = norm_name(&e.name);
            all_members
                .entry(name.clone())
                .or_default()
                .push(archive.to_owned());
            st.members += 1;
            st.bytes += e.size as u64;
            *st.exts.entry(ext_of(&name)).or_insert(0) += 1;
            let data = match pck::read_entry(&mut f, e) {
                Ok(d) => d,
                Err(err) => {
                    st.read_errors += 1;
                    cases.push(Case {
                        archive: archive.to_owned(),
                        member: name,
                        kind: "member-read-error".into(),
                        detail: err,
                    });
                    continue;
                }
            };
            let class = classify(&data, &name);
            *st.classes.entry(class.clone()).or_insert(0) += 1;
            *st.class_bytes.entry(class.clone()).or_insert(0) += data.len() as u64;
            match class.as_str() {
                "bmd-moxb" => {
                    walk_bmd(
                        &data,
                        &mut st.bmd,
                        &mut cases,
                        archive,
                        &name,
                        &mut tex_refs,
                        &mut nrf_refs,
                    );
                    match bmd::Model::parse(&data) {
                        Ok(_) => *st.parse_results.entry("ok".into()).or_insert(0) += 1,
                        Err(err) => {
                            let key = format!("err: {err}");
                            let key = if key.len() > 120 {
                                format!("{}...", &key[..117])
                            } else {
                                key
                            };
                            *st.parse_results.entry(key).or_insert(0) += 1;
                        }
                    }
                }
                "bmd-extension-other-magic" => {
                    cases.push(Case {
                        archive: archive.to_owned(),
                        member: name.clone(),
                        kind: "bmd-extension-other-magic".into(),
                        detail: format!("first bytes {:02x?}", &data[..8.min(data.len())]),
                    });
                }
                "dds" | "bmp" | "png" | "jpg" | "tga-by-extension" => {
                    st.non_bmd_textures.insert(name);
                }
                _ => {}
            }
        }
        archives.push(st);
    }

    // ---- texture reference resolution census (P3.03 evidence) ----
    let building_set: HashSet<String> = all_members
        .iter()
        .filter(|(_, v)| v.iter().any(|a| a == "building.pck"))
        .map(|(k, _)| k.clone())
        .collect();
    let lit_set: HashSet<String> = all_members
        .iter()
        .filter(|(_, v)| v.iter().any(|a| a == "litmodels.pck"))
        .map(|(k, _)| k.clone())
        .collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for t in &tex_refs {
        *counts.entry(t.clone()).or_insert(0) += 1;
    }
    let mut empty_refs = 0usize;
    let mut sdr_refs = 0usize;
    let mut nrf_named = 0usize;
    let mut short_refs = 0usize;
    let mut ext_hist: BTreeMap<String, usize> = BTreeMap::new();
    let mut resolved_building_only = 0usize;
    let mut resolved_lit_only = 0usize;
    let mut resolved_both = 0usize;
    let mut resolved_none = 0usize;
    let mut missing_examples: Vec<String> = Vec::new();
    let mut both_examples: Vec<String> = Vec::new();
    let mut shared: Vec<(String, usize)> = Vec::new();
    for (name, n) in &counts {
        if name.is_empty() {
            empty_refs += n;
            continue;
        }
        if name.contains(".sdr.") {
            sdr_refs += n;
        }
        if name.contains("nrf_") {
            nrf_named += n;
        }
        if name.chars().count() <= 4 {
            short_refs += n;
        }
        *ext_hist.entry(ext_of(name)).or_insert(0) += n;
        if *n > 1 {
            shared.push((name.clone(), *n));
        }
        let in_b = building_set.contains(name);
        let in_l = lit_set.contains(name);
        match (in_b, in_l) {
            (true, true) => {
                resolved_both += 1;
                if both_examples.len() < 20 {
                    both_examples.push(name.clone());
                }
            }
            (true, false) => resolved_building_only += 1,
            (false, true) => resolved_lit_only += 1,
            (false, false) => {
                resolved_none += 1;
                if missing_examples.len() < 40 {
                    missing_examples.push(format!("{name} ({n} refs)"));
                }
            }
        }
    }
    shared.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut nrf_present = 0usize;
    let mut nrf_missing = 0usize;
    let mut nrf_missing_examples: Vec<String> = Vec::new();
    let mut nrf_unique: BTreeMap<String, usize> = BTreeMap::new();
    for (_, derived) in &nrf_refs {
        *nrf_unique.entry(derived.clone()).or_insert(0) += 1;
    }
    for (derived, n) in &nrf_unique {
        if all_members.contains_key(derived.as_str()) {
            nrf_present += n;
        } else {
            nrf_missing += n;
            if nrf_missing_examples.len() < 10 {
                nrf_missing_examples.push(format!("{derived} ({n} refs)"));
            }
        }
    }

    // ---- report ----
    let mut md = String::new();
    md.push_str("# Phase 3 Agent A — BMD corpus census (litmodels.pck + building.pck)\n\n");
    md.push_str("Generated by `src/bin/bmd-corpus-census.rs`. Native references: ");
    md.push_str("AFileImage::Open 0x76da20 (MOXB/MOXT sniff 0x76da7c/0x76daa1), ");
    md.push_str(
        "A3DLitModel::Load 0x7bb380 (versions 0x10000001/0x10000002 at 0x7bb3de/0x7bb58b, ",
    );
    md.push_str(
        "\"Wrong version!\" 0x7bb79e), A3DLitMesh::Load 0x7ba070 (v1 silent-false 0x7baa64, ",
    );
    md.push_str(
        "v2 0x7ba0b8, v3 0x7ba30d, v4 0x7ba573, v5 0x7ba7a7, \"Unknown version\" 0x7baa51), ",
    );
    md.push_str("A3DLitMesh::LoadTexture 0x7b9880 (nrf derivation 0x7b98e8-0x7b9944, ");
    md.push_str("existence check 0x76f1e0), A3DTextureMan::LoadTexture 0x77c230.\n\n");
    for st in &archives {
        md.push_str(&format!("## {}\n\n", st.name));
        md.push_str(&format!(
            "- members: {} ({:.1} MiB uncompressed), read errors: {}\n",
            st.members,
            st.bytes as f64 / 1048576.0,
            st.read_errors
        ));
        md.push_str(
            "- member classes (magic sniff):\n\n| class | members | MiB |\n|---|---|---|\n",
        );
        for (c, n) in &st.classes {
            md.push_str(&format!(
                "| {c} | {n} | {:.1} |\n",
                st.class_bytes.get(c).copied().unwrap_or(0) as f64 / 1048576.0
            ));
        }
        md.push_str("\n- extensions:\n\n| ext | members |\n|---|---|\n");
        for (e, n) in st
            .exts
            .iter()
            .rev()
            .take(25)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            md.push_str(&format!("| {e} | {n} |\n"));
        }
        let b = &st.bmd;
        if b.files > 0 {
            md.push_str(&format!(
                "\n### BMD walk ({} files)\n\n- chunk wrapper 0x80000001: {} files (byte@8 histogram: {:?})\n",
                b.files, b.wrapper, b.wrapper_byte8
            ));
            md.push_str("- model versions:\n\n| version | files |\n|---|---|\n");
            for (v, n) in &b.model_versions {
                md.push_str(&format!("| {v:#010x} | {n} |\n"));
            }
            md.push_str("\n- mesh versions:\n\n| version | meshes |\n|---|---|\n");
            for (v, n) in &b.mesh_versions {
                md.push_str(&format!("| {v:#010x} | {n} |\n"));
            }
            md.push_str(&format!(
                "\n- meshes: {}, vertices: {}, faces: {}, v5 materials: {}, two-sided: {}, truncated meshes: {}, files with stopped scan: {}\n",
                b.meshes, b.vertices, b.faces, b.with_material, b.two_sided, b.truncated_meshes, b.stopped_scan
            ));
            md.push_str(&format!(
                "- distinct material names: {} (top: {:?})\n",
                b.material_names.len(),
                b.material_names.iter().take(5).collect::<Vec<_>>()
            ));
            md.push_str(&format!(
                "- trailer (scenery light attachments, loader 0x64e200 -> 0x64dd30/0x64e010 -> 0x72a0a0): {} files with trailer, {} groups, {} group-member ids, {} light records, flag-byte histogram {:?}. Ids are NOT mesh indices ({} of them exceed the file's mesh count; max id {}); their meaning belongs to the scenery light-attachment system, outside Agent A's lane.\n",
                b.trailer_files, b.trailer_groups, b.trailer_ids, b.trailer_lights, b.trailer_flag_bytes, b.trailer_bad_ids, b.trailer_max_id
            ));
        }
        md.push_str("\n### src/bmd.rs parse coverage on this archive's BMDs\n\n| result | files |\n|---|---|\n");
        for (r, n) in &st.parse_results {
            md.push_str(&format!("| {r} | {n} |\n"));
        }
        md.push('\n');
    }
    md.push_str("## Texture references from BMD meshes (P3.03)\n\n");
    md.push_str(&format!(
        "- total references: {} ({} distinct names)\n- empty names: {} (client: no texture handle, 0x77c260)\n",
        tex_refs.len(),
        counts.len(),
        empty_refs
    ));
    md.push_str(&format!(
        "- names containing \".sdr.\" (shader-registry textures, 0x77c2e4): {sdr_refs}\n- names containing \"nrf_\" (skip derivation, 0x7b98ee): {nrf_named}\n- names with <= 4 chars (skip derivation, 0x7b98df): {short_refs}\n"
    ));
    md.push_str("- reference extensions:\n\n| ext | refs |\n|---|---|\n");
    for (e, n) in &ext_hist {
        md.push_str(&format!("| {e} | {n} |\n"));
    }
    md.push_str(&format!(
        "\n- resolution against the staged corpus (distinct names): building.pck only {resolved_building_only}, litmodels.pck only {resolved_lit_only}, BOTH archives {resolved_both}, neither {resolved_none}\n"
    ));
    if !both_examples.is_empty() {
        md.push_str(&format!(
            "- names present in both archives (order matters): {:?}\n",
            both_examples
        ));
    }
    if !missing_examples.is_empty() {
        md.push_str(&format!("- unresolved examples: {:?}\n", missing_examples));
    }
    md.push_str(&format!(
        "- shared references (same name used by >1 mesh): {} names, top: {:?}\n",
        shared.len(),
        &shared[..shared.len().min(10)]
    ));
    md.push_str(&format!(
        "- nrf-derived second textures: {} references ({} distinct): present in corpus {nrf_present}, absent {nrf_missing}\n",
        nrf_refs.len(),
        nrf_unique.len()
    ));
    if !nrf_missing_examples.is_empty() {
        md.push_str(&format!(
            "- absent nrf examples (shaders.pck not staged): {:?}\n",
            nrf_missing_examples
        ));
    }
    md.push_str(&format!(
        "\n## Special cases ({} total, each with source member)\n\n| archive | member | kind | detail |\n|---|---|---|---|\n",
        cases.len()
    ));
    let cap = 400;
    for c in cases.iter().take(cap) {
        md.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            c.archive,
            c.member,
            c.kind,
            c.detail.replace('|', "/")
        ));
    }
    if cases.len() > cap {
        md.push_str(&format!(
            "\n(... {} more cases truncated from this table ...)\n",
            cases.len() - cap
        ));
    }
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for c in &cases {
        *kinds.entry(c.kind.clone()).or_insert(0) += 1;
    }
    md.push_str("\nCase kinds:\n\n| kind | count |\n|---|---|\n");
    for (k, n) in &kinds {
        md.push_str(&format!("| {k} | {n} |\n"));
    }
    std::fs::write(&out, &md).map_err(|e| e.to_string())?;
    println!("wrote {out}");
    println!(
        "members: {}",
        archives.iter().map(|s| s.members).sum::<usize>()
    );
    for st in &archives {
        println!(
            "{}: {} BMD files, model versions {:?}, mesh versions {:?}",
            st.name, st.bmd.files, st.bmd.model_versions, st.bmd.mesh_versions
        );
    }
    println!(
        "texture refs: {} ({} distinct; building-only {resolved_building_only}, lit-only {resolved_lit_only}, both {resolved_both}, missing {resolved_none}); cases: {}",
        tex_refs.len(),
        counts.len(),
        cases.len()
    );
    Ok(())
}

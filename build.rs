use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

fn find_dxc() -> Option<PathBuf> {
    if let Some(explicit) = env::var_os("DXC_PATH") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    if Command::new("dxc.exe")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Some(PathBuf::from("dxc.exe"));
    }
    let mut roots = Vec::new();
    if let Some(sdk) = env::var_os("WindowsSdkDir") {
        roots.push(PathBuf::from(sdk).join("bin"));
    }
    roots.push(PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin"));
    roots.push(PathBuf::from(r"C:\Program Files\Windows Kits\10\bin"));

    let mut candidates: Vec<PathBuf> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let dxc = entry.path().join("x64").join("dxc.exe");
            if dxc.is_file() {
                candidates.push(dxc);
            }
        }
        let direct = root.join("x64").join("dxc.exe");
        if direct.is_file() {
            candidates.push(direct);
        }
    }
    candidates.sort();
    candidates.pop()
}

fn compile_dxr_shader(manifest_dir: &Path, out: &Path) {
    let hlsl = manifest_dir.join("src/modules/raywatcher/dxr_trace.hlsl");
    if !hlsl.is_file() {
        return;
    }
    let Some(dxc) = find_dxc() else {
        println!(
            "cargo:warning=dxc.exe not found; Raywatcher DXR 1.1 hardware ray tracing shader skipped (Software mode will be used)"
        );
        return;
    };
    let dxil = out.join("dxr_trace.dxil");
    let status = Command::new(&dxc)
        .args(["-T", "cs_6_5", "-E", "trace_pass", "-O3", "-Fo"])
        .arg(&dxil)
        .arg(&hlsl)
        .status()
        .expect("failed to invoke dxc.exe");
    assert!(
        status.success(),
        "dxc.exe failed to compile src/modules/raywatcher/dxr_trace.hlsl"
    );
    println!("cargo:rustc-cfg=pwe_dxr_shader");
}

fn main() {
    println!("cargo:rustc-check-cfg=cfg(pwe_directstorage)");
    println!("cargo:rustc-check-cfg=cfg(pwe_dxr_shader)");
    println!("cargo:rerun-if-env-changed=DIRECTSTORAGE_SDK_DIR");
    println!("cargo:rerun-if-env-changed=DXC_PATH");
    println!("cargo:rerun-if-changed=src/directstorage_shim.cpp");
    println!("cargo:rerun-if-changed=src/modules/raywatcher/dxr_trace.hlsl");

    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("windows-msvc") || !cfg!(target_os = "windows") {
        return;
    }
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));

    // Compile the DXR 1.1 RayQuery shader on Windows MSVC hosts so Hardware RT
    // mode executes directly on RT cores (RTX, RDNA2+, Arc).
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") && cfg!(target_os = "windows") {
        compile_dxr_shader(&manifest_dir, &out);
    }

    let sdk = env::var_os("DIRECTSTORAGE_SDK_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                manifest_dir.join(path)
            }
        })
        .unwrap_or_else(|| manifest_dir.join("sdk/DirectStorage/1.4.0-preview2-2606.904"));
    if !sdk.join("native/include/dstorage.h").is_file() {
        println!("cargo:warning=DirectStorage SDK not found at {}; run tools/fetch-directstorage.ps1 or set DIRECTSTORAGE_SDK_DIR. Using standard file I/O.", sdk.display());
        return;
    }

    let include = sdk.join("native/include");
    let include_alt = sdk.join("include");
    let header_dir = if include.join("dstorage.h").is_file() {
        include
    } else if include_alt.join("dstorage.h").is_file() {
        include_alt
    } else {
        panic!("DIRECTSTORAGE_SDK_DIR does not contain native/include/dstorage.h (or include/dstorage.h): {}", sdk.display());
    };
    println!(
        "cargo:rerun-if-changed={}",
        header_dir.join("dstorage.h").display()
    );

    let object = out.join("directstorage_shim.obj");
    let library = out.join("pwe_directstorage_shim.lib");
    let source = manifest_dir.join("src/directstorage_shim.cpp");

    let status = Command::new("cl.exe")
        .args(["/nologo", "/c", "/EHsc", "/std:c++17", "/W4", "/O2"])
        .arg(format!("/I{}", header_dir.display()))
        .arg(format!("/Fo{}", object.display()))
        .arg(&source)
        .status()
        .expect("DirectStorage enabled but cl.exe was not found; build from a Visual Studio developer environment");
    assert!(
        status.success(),
        "failed to compile the DirectStorage bridge"
    );

    let status = Command::new("lib.exe")
        .arg("/nologo")
        .arg(format!("/OUT:{}", library.display()))
        .arg(&object)
        .status()
        .expect("DirectStorage enabled but lib.exe was not found");
    assert!(
        status.success(),
        "failed to archive the DirectStorage bridge"
    );

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=pwe_directstorage_shim");
    println!(
        "cargo:rustc-env=PWE_DIRECTSTORAGE_RUNTIME_DIR={}",
        sdk.join("native/bin/x64").display()
    );
    println!("cargo:rustc-cfg=pwe_directstorage");
}

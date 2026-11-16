use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(pwe_directstorage)");
    println!("cargo:rerun-if-env-changed=DIRECTSTORAGE_SDK_DIR");
    println!("cargo:rerun-if-changed=src/directstorage_shim.cpp");

    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("windows-msvc") {
        return;
    }
    let Some(sdk) = env::var_os("DIRECTSTORAGE_SDK_DIR").map(PathBuf::from) else {
        println!("cargo:warning=DirectStorage SDK not configured; using standard file I/O. Set DIRECTSTORAGE_SDK_DIR to the extracted Microsoft.Direct3D.DirectStorage package root to enable it.");
        return;
    };

    let include = sdk.join("native/include");
    let include_alt = sdk.join("include");
    let header_dir = if include.join("dstorage.h").is_file() {
        include
    } else if include_alt.join("dstorage.h").is_file() {
        include_alt
    } else {
        panic!("DIRECTSTORAGE_SDK_DIR does not contain native/include/dstorage.h (or include/dstorage.h): {}", sdk.display());
    };
    println!("cargo:rerun-if-changed={}", header_dir.join("dstorage.h").display());

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let object = out.join("directstorage_shim.obj");
    let library = out.join("pwe_directstorage_shim.lib");
    let source = PathBuf::from("src/directstorage_shim.cpp");

    let status = Command::new("cl.exe")
        .args(["/nologo", "/c", "/EHsc", "/std:c++17", "/W4", "/O2"])
        .arg(format!("/I{}", header_dir.display()))
        .arg(format!("/Fo{}", object.display()))
        .arg(&source)
        .status()
        .expect("DirectStorage enabled but cl.exe was not found; build from a Visual Studio developer environment");
    assert!(status.success(), "failed to compile the DirectStorage bridge");

    let status = Command::new("lib.exe")
        .arg("/nologo")
        .arg(format!("/OUT:{}", library.display()))
        .arg(&object)
        .status()
        .expect("DirectStorage enabled but lib.exe was not found");
    assert!(status.success(), "failed to archive the DirectStorage bridge");

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=pwe_directstorage_shim");
    println!("cargo:rustc-cfg=pwe_directstorage");
}

//! Identify known clients from their actual PE executable, not a folder name.
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

const PW142_SIZE: usize = 6_000_640;
const PW142_SHA256: &str = "a290998dc13cc5558b54bda3f65d099f95bc0702e54a0613c6d7669f35c6cdb0";

#[derive(Debug)]
pub struct Client {
    pub element: PathBuf,
    pub known_142: bool,
}
fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.is_file() {
        return Some(exact);
    }
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}
/// A client may ship only instance maps, so validate any readable
/// `maps/<id>/<id>.{trn2,ecwld,ecbsd}` set rather than requiring `world.*`.
fn readable_map_dir(element: &Path) -> Option<PathBuf> {
    let maps = element.join("maps");
    let mut dirs = fs::read_dir(maps)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs.into_iter().find(|dir| {
        let Some(id) = dir.file_name().and_then(OsStr::to_str) else {
            return false;
        };
        let stem = if find_file(dir, &format!("{id}.ecbsd")).is_some() {
            id
        } else {
            "world"
        };
        ["trn2", "ecwld", "ecbsd"]
            .iter()
            .all(|ext| find_file(dir, &format!("{stem}.{ext}")).is_some())
    })
}
pub fn detect(selected: &Path) -> Result<Client, String> {
    let element = [selected.join("element"), selected.to_path_buf()]
        .into_iter()
        .find(|candidate| readable_map_dir(candidate).is_some())
        .unwrap_or_else(|| selected.to_path_buf());
    let map = readable_map_dir(&element).ok_or_else(|| {
        "The selected folder has no readable maps/<id>/<id>.trn2, .ecwld and .ecbsd set. Select the extracted client root or its element folder.".to_string()
    })?;
    let id = map.file_name().and_then(OsStr::to_str).unwrap_or("world");
    let stem = if find_file(&map, &format!("{id}.ecbsd")).is_some() {
        id
    } else {
        "world"
    };
    for path in [
        "elementclient.exe",
        "trees.pck",
        "building.pck",
        "textures.pck",
        "speedtreert.dll",
    ] {
        if !element.join(path).is_file() {
            return Err(format!("The selected folder is missing {path}. Select the extracted client root or its element folder."));
        }
    }
    for ext in ["trn2", "ecwld", "ecbsd"] {
        if find_file(&map, &format!("{stem}.{ext}")).is_none() {
            return Err(format!(
                "The selected folder is missing maps/{id}/{stem}.{ext}."
            ));
        }
    }
    let bytes = fs::read(element.join("elementclient.exe"))
        .map_err(|e| format!("Cannot read elementclient.exe: {e}"))?;
    let pe = goblin::pe::PE::parse(&bytes)
        .map_err(|e| format!("elementclient.exe is not a valid Windows PE executable: {e}"))?;
    if pe.header.coff_header.machine != 0x14c || pe.is_64 {
        return Err("elementclient.exe is not a 32-bit x86 game client".into());
    }
    let known_142 = bytes.len() == PW142_SIZE && digest(&bytes) == PW142_SHA256;
    Ok(Client { element, known_142 })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hash_reference() {
        assert_eq!(
            digest(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
    #[test]
    fn rejects_missing() {
        assert!(detect(Path::new("__not_a_client__")).is_err());
    }

    #[test]
    fn readable_map_dir_accepts_instance_files_and_case_insensitive_names() {
        let root =
            std::env::temp_dir().join(format!("pw-client-map-detect-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let a01 = root.join("maps/a01");
        fs::create_dir_all(&a01).unwrap();
        for ext in ["trn2", "ecwld"] {
            fs::write(a01.join(format!("a01.{ext}")), []).unwrap();
        }
        fs::write(a01.join("A01.ECBSD"), []).unwrap();
        assert_eq!(readable_map_dir(&root), Some(a01));
        fs::remove_dir_all(root).unwrap();
    }
}

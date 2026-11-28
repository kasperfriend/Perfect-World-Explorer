//! SpeedTree version identification, for the DLL and for `.spt` sources.
//!
//! The `.spt` container starts with `__IdvSpt_0` followed by the version tag the
//! writer stamped, e.g. `2_`. The runtime DLL holds the same literal, so the
//! tag a DLL accepts can be read out of the DLL's own bytes rather than guessed
//! from a list. Both staged clients agree: their `speedtreert.dll` (261,120 B,
//! PE timestamp 2020-01-01, and 454,792 B, PE timestamp 2005-07-28) both embed
//! `__IdvSpt_02_`, and every one of the 100 `.spt` members of `trees.pck` is
//! `__IdvSpt_02_`.
//!
//! Reading it is cheap and it is the right check: the DLL rejects a source it
//! cannot parse, and a mismatch here is exactly the case where a client ships a
//! newer `trees.pck` beside an older runtime. A mismatch is **warned about, not
//! refused**, because the DLL itself is the authority on what it accepts: the
//! tag is a header string, and a DLL that reads a compatible format is entitled
//! to load a tree whose tag it did not expect. The warning names both tags so a
//! real load failure is not blamed on the wrong thing.

/// The literal every `.spt` container and every SpeedTreeRT build carries.
const SPT_MAGIC: &[u8] = b"__IdvSpt_0";

/// The tag a `.spt` byte stream declares, e.g. `"2_"`.
///
/// Returns `None` when the literal is absent, which means the file is not a
/// SpeedTree source at all rather than a source of another version.
pub fn spt_tag(bytes: &[u8]) -> Option<String> {
    let at = find(bytes, SPT_MAGIC)? + SPT_MAGIC.len();
    let rest = bytes.get(at..)?;
    let end = rest
        .iter()
        .position(|b| !b.is_ascii_graphic())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    std::str::from_utf8(&rest[..end]).ok().map(str::to_owned)
}

/// The tag a SpeedTreeRT DLL embeds, e.g. `"2_"`.
///
/// The DLL keeps the literal in its read-only data, not in a resource, and the
/// PE has no version resource at all in either staged build (`VS_VERSIONINFO`
/// is absent), so this string is the only version marker available.
pub fn dll_tag(bytes: &[u8]) -> Option<String> {
    spt_tag(bytes)
}

/// PE machine types the SpeedTree bridge can load.
///
/// The bridge the viewer writes out is a 32-bit process, so an x64 or ARM
/// `speedtreert.dll` cannot be loaded into it whatever else is true of it.
pub const PE_MACHINE_I386: u16 = 0x014c;

/// The `IMAGE_FILE_HEADER.Machine` word of a PE byte stream.
pub fn pe_machine(bytes: &[u8]) -> Option<u16> {
    let at = usize::try_from(u32::from_le_bytes(bytes.get(0x3c..0x40)?.try_into().ok()?)).ok()?;
    if bytes.get(at..at + 4)? != b"PE\0\0" {
        return None;
    }
    Some(u16::from_le_bytes(
        bytes.get(at + 4..at + 6)?.try_into().ok()?,
    ))
}

/// A human name for a PE machine word, for the refusal message.
pub fn machine_name(machine: u16) -> &'static str {
    match machine {
        0x014c => "x86 (i386)",
        0x8664 => "x64",
        0x01c0 => "ARM",
        0x01c4 => "ARMv7 (Thumb-2)",
        0xaa64 => "ARM64",
        0x0200 => "Itanium",
        _ => "an unrecognised machine type",
    }
}

/// The bytes that count as the tree's identity for the cache key: a subslice,
/// returned only when the DLL is a usable 32-bit PE.
///
/// Reserving this for the bridge's own check keeps the "why" in one place: a
/// caller that cannot load the DLL gets a sentence naming the machine word
/// instead of a loader error much later.
pub fn check_dll(machine: Option<u16>) -> Result<(), String> {
    match machine {
        Some(PE_MACHINE_I386) => Ok(()),
        Some(other) => Err(format!(
            "speedtreert.dll is {} ({other:#06x}); the 32-bit SpeedTree bridge can only load \
             x86 (i386, {PE_MACHINE_I386:#06x})",
            machine_name(other)
        )),
        None => Err(
            "speedtreert.dll is not a PE image: no PE header where the DOS stub points".to_owned(),
        ),
    }
}

/// The warning line for a DLL/source tag pair, when they differ.
///
/// `None` means there is nothing to say: both tags read and match, or one could
/// not be read at all and the DLL's own loader is the only authority.
pub fn tag_warning(dll: Option<&str>, spt: Option<&str>) -> Option<String> {
    let (dll, spt) = (dll?, spt?);
    if dll == spt {
        return None;
    }
    Some(format!(
        "SpeedTree version mismatch: speedtreert.dll expects __IdvSpt_0{dll} but this source is \
         __IdvSpt_0{spt}; trees are kept and the runtime decides"
    ))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spt(tag: &str) -> Vec<u8> {
        let mut b = vec![0xe8, 0x03, 0, 0, 0x0c, 0, 0, 0];
        b.extend_from_slice(SPT_MAGIC);
        b.extend_from_slice(tag.as_bytes());
        b.push(0);
        b.extend_from_slice(&[0u8; 8]);
        b
    }

    #[test]
    fn reads_the_tag_from_a_source() {
        // The staged corpus is uniformly this tag.
        assert_eq!(spt_tag(&spt("2_")), Some("2_".into()));
        assert_eq!(spt_tag(&spt("3_")), Some("3_".into()));
    }

    #[test]
    fn a_file_without_the_literal_is_not_a_speedtree_source() {
        assert_eq!(spt_tag(b"not a tree"), None);
        assert_eq!(spt_tag(b"__IdvSpt_0"), None);
    }

    #[test]
    fn reads_the_pe_machine_word() {
        let mut b = vec![0u8; 0x40];
        b[0..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b.resize(0x88, 0);
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x84..0x86].copy_from_slice(&PE_MACHINE_I386.to_le_bytes());
        assert_eq!(pe_machine(&b), Some(PE_MACHINE_I386));
        assert_eq!(check_dll(pe_machine(&b)), Ok(()));
        b[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        let message = check_dll(pe_machine(&b)).unwrap_err();
        assert!(message.contains("x64"), "{message}");
        // A truncated or non-PE stream is named as such, not as a machine type.
        assert!(check_dll(pe_machine(b"short")).unwrap_err().contains("PE"));
    }

    #[test]
    fn a_dll_and_a_source_are_compared() {
        assert_eq!(tag_warning(Some("2_"), Some("2_")), None);
        let warning = tag_warning(Some("2_"), Some("3_")).unwrap();
        assert!(warning.contains("__IdvSpt_02_") && warning.contains("__IdvSpt_03_"));
        // Unknown on either side says nothing.
        assert_eq!(tag_warning(None, Some("3_")), None);
        assert_eq!(tag_warning(Some("2_"), None), None);
    }
}

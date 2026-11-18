//! Bounds-checked walker for the PW 1.4.2 elements.data layout.

const ELEMENTS_V27: u32 = 0x3000_001b;

/// Why a file cannot be read as the v27 layout: which version it says it is,
/// and how that differs from v27. The header packs the family marker in the
/// high half and the version number in the low half (`0x3000_001b` is v27),
/// so a different release is named by its own number rather than reported as
/// a bare hex word.
pub(crate) fn unsupported_layout(version: u32) -> String {
    let number = version & 0xffff;
    let family = version >> 16;
    if version == ELEMENTS_V27 {
        return "elements.data v27 is the supported layout".into();
    }
    if family != 0x3000 {
        return format!(
            "elements.data 0x{version:08x} is not an elements file this build knows \
             (family 0x{family:04x}, version {number}); the v27 reader expects family 0x3000"
        );
    }
    format!(
        "elements.data is v{number} (0x{version:08x}), not the v27 layout this build reads; \
         the record tables are version-specific and are not guessed"
    )
}

/// The version number in an `elements.data` header, or `None` when the file is
/// too short. Used by the folder scan and by the diagnostics below; it reads
/// nothing else, so it works on any release.
pub(crate) fn elements_version(bytes: &[u8]) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw) & 0xffff)
}

pub(crate) fn visit_v27_records(
    bytes: &[u8],
    mut visit: impl FnMut(usize, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let mut cursor = Cursor { bytes, pos: 0 };
    let version = cursor.u32()?;
    if version != ELEMENTS_V27 {
        return Err(unsupported_layout(version));
    }
    let _timestamp = cursor.u32()?;
    for (section, size) in SECTION_SIZES.iter().copied().enumerate() {
        match size {
            Size::Fixed(record_size) => {
                let count = cursor.count(record_size)?;
                for _ in 0..count {
                    visit(section, cursor.bytes(record_size)?)?;
                }
            }
            Size::ControlTimestamp => {
                let _tag = cursor.u32()?;
                let length = cursor.u32()? as usize;
                cursor.skip(length)?;
                let _timestamp = cursor.u32()?;
            }
            Size::ControlPlain => {
                let _tag = cursor.u32()?;
                let length = cursor.u32()? as usize;
                cursor.skip(length)?;
            }
            Size::Talk => {
                let count = cursor.count(1)?;
                for _ in 0..count {
                    skip_talk(&mut cursor)?;
                }
            }
        }
    }
    if cursor.pos != bytes.len() {
        return Err(format!(
            "elements.data has {} unread trailing bytes",
            bytes.len() - cursor.pos
        ));
    }
    Ok(())
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| format!("truncated v27 field at {offset}"))?;
    Ok(u32::from_le_bytes(raw.try_into().unwrap()))
}
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl Cursor<'_> {
    fn bytes(&mut self, length: usize) -> Result<&[u8], String> {
        let end = self
            .pos
            .checked_add(length)
            .ok_or("elements.data overflow")?;
        let result = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| format!("truncated elements.data at {}", self.pos))?;
        self.pos = end;
        Ok(result)
    }
    fn skip(&mut self, length: usize) -> Result<(), String> {
        self.bytes(length).map(|_| ())
    }
    fn u32(&mut self) -> Result<u32, String> {
        u32_at(self.bytes(4)?, 0)
    }
    fn count(&mut self, record_size: usize) -> Result<usize, String> {
        let count = self.u32()? as usize;
        let remaining = self.bytes.len() - self.pos;
        if record_size != 0 && count > remaining / record_size {
            return Err(format!("impossible elements.data record count {count}"));
        }
        Ok(count)
    }
}
fn skip_talk(cursor: &mut Cursor<'_>) -> Result<(), String> {
    let _id = cursor.u32()?;
    cursor.skip(128)?;
    let windows = cursor.count(1)?;
    for _ in 0..windows {
        let _id = cursor.u32()?;
        let _parent = cursor.u32()?;
        let talk_len = cursor.u32()? as usize;
        cursor.skip(talk_len.checked_mul(2).ok_or("talk text overflow")?)?;
        let options = cursor.count(136)?;
        cursor.skip(options.checked_mul(136).ok_or("talk options overflow")?)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Size {
    Fixed(usize),
    ControlTimestamp,
    ControlPlain,
    Talk,
}
use Size::{ControlPlain as Plain, ControlTimestamp as Timestamp, Fixed as F, Talk as T};
const SECTION_SIZES: [Size; 131] = [
    F(84),
    F(68),
    F(356),
    F(1412),
    F(68),
    F(72),
    F(1120),
    F(68),
    F(72),
    F(1164),
    F(68),
    F(68),
    F(376),
    F(68),
    F(68),
    F(368),
    F(68),
    F(364),
    F(68),
    F(624),
    Timestamp,
    F(68),
    F(348),
    F(524),
    F(488),
    F(348),
    F(348),
    F(352),
    F(348),
    F(208),
    F(888),
    F(68),
    F(892),
    F(68),
    F(340),
    F(68),
    F(436),
    F(84),
    F(196),
    F(1528),
    F(72),
    F(1224),
    F(72),
    F(72),
    F(200),
    F(200),
    F(1092),
    F(1092),
    F(644),
    F(584),
    F(72),
    F(460),
    F(328),
    F(72),
    F(68),
    F(1228),
    F(72),
    F(68),
    F(852),
    T,
    F(476),
    F(348),
    F(196),
    F(336),
    F(468),
    F(340),
    F(208),
    F(204),
    F(68),
    F(68),
    F(416),
    F(196),
    F(160),
    F(612),
    F(552),
    F(404),
    F(344),
    F(340),
    F(668),
    F(68),
    F(452),
    F(72),
    F(68),
    F(72),
    F(412),
    F(68),
    F(68),
    F(488),
    F(68),
    F(68),
    F(2412),
    F(292),
    F(68),
    F(344),
    F(68),
    F(480),
    F(628),
    F(360),
    F(344),
    F(480),
    F(344),
    Plain,
    F(148),
    F(1092),
    F(368),
    F(76),
    F(584),
    F(76),
    F(356),
    F(436),
    F(344),
    F(76),
    F(76),
    F(76),
    F(392),
    F(348),
    F(356),
    F(356),
    F(348),
    F(344),
    F(368),
    F(1356),
    F(68),
    F(392),
    F(352),
    F(360),
    F(384),
    F(344),
    F(644),
    F(2116),
    F(1092),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_file() -> Vec<u8> {
        let mut bytes = ELEMENTS_V27.to_le_bytes().to_vec();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        for size in SECTION_SIZES {
            match size {
                Size::Fixed(_) | Size::Talk => bytes.extend_from_slice(&0u32.to_le_bytes()),
                Size::ControlTimestamp => bytes.extend_from_slice(&[0; 12]),
                Size::ControlPlain => bytes.extend_from_slice(&[0; 8]),
            }
        }
        bytes
    }

    #[test]
    fn validates_complete_file_and_rejects_trailing_or_truncated_data() {
        let bytes = empty_file();
        assert!(visit_v27_records(&bytes, |_, _| panic!("unexpected record")).is_ok());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(visit_v27_records(&trailing, |_, _| Ok(())).is_err());
        assert!(visit_v27_records(&bytes[..bytes.len() - 1], |_, _| Ok(())).is_err());
    }
}

/// A complete file whose only contents are the given `(section, records)`
/// pairs. Every other section stays empty, so a parser reads exactly what the
/// caller wrote and nothing else.
#[cfg(test)]
pub(super) fn synthetic_records(sections: &[(usize, Vec<Vec<u8>>)]) -> Vec<u8> {
    let mut bytes = ELEMENTS_V27.to_le_bytes().to_vec();
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for (index, size) in SECTION_SIZES.iter().copied().enumerate() {
        match size {
            Size::Fixed(size) => {
                let records = sections
                    .iter()
                    .find(|(section, _)| *section == index)
                    .map(|(_, records)| records.as_slice())
                    .unwrap_or(&[]);
                bytes.extend_from_slice(&(records.len() as u32).to_le_bytes());
                for record in records {
                    assert_eq!(record.len(), size, "section {index} record size");
                    bytes.extend_from_slice(record);
                }
            }
            Size::Talk => bytes.extend_from_slice(&0u32.to_le_bytes()),
            Size::ControlTimestamp => bytes.extend_from_slice(&[0; 12]),
            Size::ControlPlain => bytes.extend_from_slice(&[0; 8]),
        }
    }
    bytes
}

#[cfg(test)]
pub(super) fn synthetic_file(section_index: usize, record: &[u8]) -> Vec<u8> {
    let mut bytes = ELEMENTS_V27.to_le_bytes().to_vec();
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for (index, size) in SECTION_SIZES.iter().copied().enumerate() {
        match size {
            Size::Fixed(size) => {
                let present = index == section_index;
                bytes.extend_from_slice(&(present as u32).to_le_bytes());
                if present {
                    assert_eq!(record.len(), size);
                    bytes.extend_from_slice(record);
                }
            }
            Size::Talk => bytes.extend_from_slice(&0u32.to_le_bytes()),
            Size::ControlTimestamp => bytes.extend_from_slice(&[0; 12]),
            Size::ControlPlain => bytes.extend_from_slice(&[0; 8]),
        }
    }
    bytes
}

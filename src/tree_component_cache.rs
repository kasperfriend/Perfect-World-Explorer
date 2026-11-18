//! Experimental PWCOMP01 independent component geometry reader.
//!
//! This is a diagnostic archive, not a renderer-ready cache: leaf card corners
//! are for a fixed camera, and wind, material state and LOD transitions are not
//! included. It is deliberately not wired into normal viewer startup.

#[derive(Debug, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}
#[derive(Debug, PartialEq)]
pub struct Part {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}
#[derive(Debug, PartialEq)]
pub struct Leaf {
    pub center: [f32; 3],
    pub normal: [f32; 3],
    pub cluster: u32,
    pub uv: [f32; 8],
    /// Four 4-float entries as emitted by the DLL for camera direction +X.
    pub reference_card: [f32; 16],
}
#[derive(Debug, PartialEq)]
pub struct Components {
    pub branch_texture: Vec<u8>,
    pub composite_texture: Vec<u8>,
    pub branches: Vec<Part>,
    pub fronds: Vec<Part>,
    pub leaves: Vec<Vec<Leaf>>,
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.offset.checked_add(n).ok_or("offset overflow")?;
        let bytes = self.data.get(self.offset..end).ok_or("truncated cache")?;
        self.offset = end;
        Ok(bytes)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn float(&mut self) -> Result<f32, String> {
        let v = f32::from_bits(self.u32()?);
        if !v.is_finite() {
            return Err("nonfinite geometry".into());
        }
        Ok(v)
    }
    fn floats<const N: usize>(&mut self) -> Result<[f32; N], String> {
        let mut values = [0.; N];
        for value in &mut values {
            *value = self.float()?;
        }
        Ok(values)
    }
    fn remaining_at_least(&self, n: usize) -> Result<(), String> {
        if n > self.data.len() - self.offset {
            return Err("truncated cache".into());
        }
        Ok(())
    }
    fn text(&mut self) -> Result<Vec<u8>, String> {
        let n = self.u32()? as usize;
        if n > 4096 {
            return Err("texture name too long".into());
        }
        Ok(self.take(n)?.to_vec())
    }
    fn levels(&mut self) -> Result<usize, String> {
        let count = self.u32()? as usize;
        if count > 64 {
            return Err("unsupported component LOD count".into());
        }
        Ok(count)
    }
    fn part(&mut self) -> Result<Part, String> {
        let count = self.u32()? as usize;
        if count > 65535 {
            return Err("vertex limit".into());
        }
        self.remaining_at_least(count * 32)?;
        let mut vertices = Vec::with_capacity(count);
        for _ in 0..count {
            vertices.push(Vertex {
                position: self.floats()?,
                normal: self.floats()?,
                uv: self.floats()?,
            });
        }
        let count = self.u32()? as usize;
        if count > 3_000_000 || count % 3 != 0 {
            return Err("index limit".into());
        }
        self.remaining_at_least(count * 4)?;
        let mut indices = Vec::with_capacity(count);
        for _ in 0..count {
            let index = self.u32()?;
            if index as usize >= vertices.len() {
                return Err("index out of range".into());
            }
            indices.push(index);
        }
        Ok(Part { vertices, indices })
    }
    fn leaf_lod(&mut self) -> Result<Vec<Leaf>, String> {
        let count = self.u32()? as usize;
        if count > 65535 {
            return Err("leaf limit".into());
        }
        self.remaining_at_least(count * 124)?;
        let mut leaves = Vec::with_capacity(count);
        for _ in 0..count {
            let center = self.floats()?;
            let normal = self.floats()?;
            let cluster = self.u32()?;
            if cluster > 255 {
                return Err("invalid leaf cluster".into());
            }
            leaves.push(Leaf {
                center,
                normal,
                cluster,
                uv: self.floats()?,
                reference_card: self.floats()?,
            });
        }
        Ok(leaves)
    }
}
fn hash(bytes: &[u8]) -> u32 {
    bytes.iter().fold(2166136261u32, |h, b| {
        (h ^ u32::from(*b)).wrapping_mul(16777619)
    })
}

/// Parse only a cache matching the exact runtime, SPT and requested tree type.
/// FNV-1a hashes identify stale caches; they are not cryptographic integrity checks.
pub fn parse(
    data: &[u8],
    source: &[u8],
    runtime: &[u8],
    tree_type: u32,
) -> Result<Components, String> {
    let mut r = Reader { data, offset: 0 };
    let mut parse_inner = || -> Result<Components, String> {
        if r.take(8)? != b"PWCOMP01" {
            return Err("unknown cache version".into());
        }
        if r.u32()? != hash(runtime) {
            return Err("runtime mismatch".into());
        }
        if r.u32()? != hash(source) {
            return Err("source mismatch".into());
        }
        if r.u32()? != tree_type {
            return Err("type mismatch".into());
        }
        let branch_texture = r.text()?;
        let composite_texture = r.text()?;
        let branch_lods = r.levels()?;
        let mut branches = Vec::with_capacity(branch_lods as usize);
        for _ in 0..branch_lods {
            branches.push(r.part().map_err(|e| format!("branch material: {e}"))?);
        }
        let frond_lods = r.levels()?;
        let mut fronds = Vec::with_capacity(frond_lods as usize);
        for _ in 0..frond_lods {
            fronds.push(r.part().map_err(|e| format!("frond material: {e}"))?);
        }
        let leaf_lods = r.levels()?;
        let mut leaves = Vec::with_capacity(leaf_lods as usize);
        for _ in 0..leaf_lods {
            leaves.push(r.leaf_lod().map_err(|e| format!("leaf material: {e}"))?);
        }
        if r.offset != data.len() {
            return Err("trailing cache bytes".into());
        }
        Ok(Components {
            branch_texture,
            composite_texture,
            branches,
            fronds,
            leaves,
        })
    };
    parse_inner().map_err(|e| format!("tree type {tree_type}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn u(b: &mut Vec<u8>, x: u32) {
        b.extend(x.to_le_bytes());
    }
    fn f(b: &mut Vec<u8>, x: f32) {
        b.extend(x.to_le_bytes());
    }
    fn fixture() -> Vec<u8> {
        let mut b = b"PWCOMP01".to_vec();
        for v in [hash(b"dll"), hash(b"spt"), 12, 0, 0, 1, 3] {
            u(&mut b, v);
        }
        for i in 0..24 {
            f(&mut b, i as f32);
        }
        for v in [3, 0, 1, 2, 1, 0, 0, 1, 1] {
            u(&mut b, v);
        }
        for i in 0..6 {
            f(&mut b, i as f32);
        }
        u(&mut b, 4);
        for i in 0..24 {
            f(&mut b, i as f32);
        }
        b
    }
    #[test]
    fn reads_independent_components() {
        let b = fixture();
        let data = parse(&b, b"spt", b"dll", 12).unwrap();
        assert_eq!(
            (data.branches.len(), data.fronds.len(), data.leaves.len()),
            (1, 1, 1)
        );
        assert_eq!(data.branches[0].indices, [0, 1, 2]);
        assert_eq!(data.leaves[0][0].cluster, 4);
        assert_eq!(data.leaves[0][0].normal, [3., 4., 5.]);
        assert_eq!(data.leaves[0][0].reference_card[15], 23.);
    }
    #[test]
    fn rejects_identity_truncation_and_trailing() {
        let mut b = fixture();
        assert!(parse(&b, b"other", b"dll", 12).is_err());
        assert!(parse(&b, b"spt", b"other", 12).is_err());
        assert!(parse(&b, b"spt", b"dll", 13).is_err());
        for n in 0..b.len() {
            assert!(parse(&b[..n], b"spt", b"dll", 12).is_err(), "prefix {n}");
        }
        b.push(0);
        assert!(parse(&b, b"spt", b"dll", 12).is_err());
    }
    #[test]
    fn rejects_invalid_geometry_with_type_and_material() {
        let mut b = fixture();
        let normal_offset = 8 + 7 * 4 + 3 * 4;
        b[normal_offset..normal_offset + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        let error = parse(&b, b"spt", b"dll", 12).unwrap_err();
        assert!(error.contains("tree type 12") && error.contains("branch material"));
        let mut b = fixture();
        let index_offset = 8 + 7 * 4 + 24 * 4 + 4;
        b[index_offset..index_offset + 4].copy_from_slice(&3u32.to_le_bytes());
        assert!(parse(&b, b"spt", b"dll", 12)
            .unwrap_err()
            .contains("index out of range"));
        let mut b = fixture();
        // First leaf center follows the branch, empty frond, and leaf count.
        let leaf_center = 8 + 7 * 4 + 24 * 4 + 9 * 4;
        b[leaf_center..leaf_center + 4].copy_from_slice(&f32::INFINITY.to_le_bytes());
        assert!(parse(&b, b"spt", b"dll", 12)
            .unwrap_err()
            .contains("leaf material"));
    }
}

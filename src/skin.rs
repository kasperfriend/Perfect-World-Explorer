//! Partial, bounds-checked A3D .ski reader (skin and rigid meshes).
//! Layout informed by GameForge 3D Studio's GPL-2.0-or-later Angelica importer:
//! https://github.com/shaddowdaniel/GameForge-3D-Studio-/tree/0a7fae8fcba0df7b0daa988db8696a0feaf49f7b/source/blender/io/angelica/formats
//! Morph, muscle and supple records intentionally return an error until implemented.
#[derive(Clone, Debug)]
pub struct Skin {
    pub version: u32,
    pub bones: Vec<String>,
    pub textures: Vec<String>,
    pub materials: Vec<Material>,
    pub meshes: Vec<Mesh>,
}
#[derive(Clone, Debug)]
pub struct Material {
    pub name: String,
    pub ambient: [f32; 4],
    pub diffuse: [f32; 4],
    pub emissive: [f32; 4],
    pub specular: [f32; 4],
    pub power: f32,
    pub two_sided: bool,
}
impl Material {
    /// Bind parsed skin material colors, not baked BMD vertex colors.
    pub fn light_constants(
        &self,
        lighting: crate::model_material::Lighting,
    ) -> crate::model_material::Constants {
        crate::model_material::bind(
            crate::model_material::Material {
                ambient: self.ambient,
                diffuse: self.diffuse,
                emissive: self.emissive,
            },
            lighting,
        )
    }
}
#[derive(Clone, Debug)]
pub struct Mesh {
    pub name: String,
    pub rigid_bone: Option<i32>,
    pub texture: i32,
    pub material: i32,
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u16>,
}
#[derive(Clone, Debug)]
pub struct Vertex {
    pub position: [f32; 3],
    pub weights: [f32; 4],
    pub joints: [u8; 4],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("size overflow")?;
        let b = self.data.get(self.pos..end).ok_or("truncated skin")?;
        self.pos = end;
        Ok(b)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    fn f32(&mut self) -> Result<f32, String> {
        let offset = self.pos;
        let bits = self.u32()?;
        let v = f32::from_bits(bits);
        if !v.is_finite() {
            return Err(format!(
                "non-finite skin value at payload byte {offset} (bits {bits:#010x})"
            ));
        }
        Ok(v)
    }
    /// A3DSkinMesh::LoadData copies the three stored blend weights verbatim.
    /// Preserve even a non-finite authored weight; retained-mesh rendering
    /// explicitly removes only triangles made non-finite by that record.
    fn weight(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn vec<const N: usize>(&mut self) -> Result<[f32; N], String> {
        let mut a = [0.; N];
        for x in &mut a {
            *x = self.f32()?;
        }
        Ok(a)
    }
    fn string(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        if n > 4096 {
            return Err("oversized skin string".into());
        }
        let b = self.bytes(n)?;
        let (s, _, bad) = encoding_rs::GBK.decode(b);
        if bad {
            return Err("invalid skin string".into());
        }
        Ok(s.trim_end_matches('\0').replace('\\', "/"))
    }
    fn cstring(&mut self) -> Result<String, String> {
        let rest = &self.data[self.pos..];
        let n = rest
            .iter()
            .position(|&c| c == 0)
            .ok_or("unterminated skin material")?;
        if n > 4096 {
            return Err("oversized material name".into());
        }
        let b = self.bytes(n + 1)?;
        let (s, _, bad) = encoding_rs::GBK.decode(&b[..n]);
        if bad {
            return Err("invalid material name".into());
        }
        Ok(s.to_string())
    }
    fn count(&mut self, max: usize) -> Result<usize, String> {
        let n = self.i32()?;
        if n < 0 || n as usize > max {
            return Err("invalid skin count".into());
        }
        Ok(n as usize)
    }
}
/// Counts declared by a validated SKI header. This is an inventory of
/// unsupported data, not an attempt to decode or partially recover that skin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SkinOmissions {
    pub files: usize,
    pub morph_skin: usize,
    pub morph_rigid: usize,
    pub supple: usize,
    pub muscle: usize,
}

impl std::fmt::Display for SkinOmissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} SKI files: morph {}/{} (skin/rigid), supple {}, muscle {}",
            self.files, self.morph_skin, self.morph_rigid, self.supple, self.muscle
        )
    }
}

#[derive(Default)]
pub struct UnsupportedSkins {
    seen: std::collections::HashSet<String>,
    totals: SkinOmissions,
}

impl UnsupportedSkins {
    pub fn observe(&mut self, path: &str, bytes: &[u8]) -> bool {
        if !(bytes.starts_with(b"IKSA")
            || bytes.starts_with(b"MOXBIKSA")
            || bytes.starts_with(b"MOXTIKSA"))
        {
            return false;
        }
        if let Ok((_, header)) = skin_header(bytes) {
            let c = header.unsupported;
            if c.files == 1 && self.seen.insert(path.replace('\\', "/").to_lowercase()) {
                self.totals.files += 1;
                self.totals.morph_skin += c.morph_skin;
                self.totals.morph_rigid += c.morph_rigid;
                self.totals.supple += c.supple;
                self.totals.muscle += c.muscle;
            }
            return c.files != 0;
        }
        false
    }

    pub fn totals(&self) -> SkinOmissions {
        self.totals
    }
}

struct Header {
    binary: bool,
    version: u32,
    skin: usize,
    rigid: usize,
    tex_count: usize,
    material_count: usize,
    bone_count: usize,
    unsupported: SkinOmissions,
}

fn skin_header(bytes: &[u8]) -> Result<(Reader<'_>, Header), String> {
    let (bytes, binary) = if bytes.starts_with(b"MOXB") {
        (&bytes[4..], true)
    } else if bytes.starts_with(b"MOXT") {
        (&bytes[4..], false)
    } else {
        (bytes, false)
    };
    let mut r = Reader {
        data: bytes,
        pos: 0,
    };
    if r.bytes(4)? != b"IKSA" {
        return Err("invalid skin signature".into());
    }
    let version = r.u32()?;
    if version == 0 || (version > 9 && version != 100 && version != 101) {
        return Err(format!("unsupported skin version {version}"));
    }
    let skin = r.count(10000)?;
    let rigid = r.count(10000)?;
    let morph_skin = r.count(10000)?;
    let morph_rigid = r.count(10000)?;
    let tex_count = r.count(10000)?;
    let material_count = r.count(10000)?;
    let bone_count = r.count(10000)?;
    let _min_weight = r.f32()?;
    let _skeleton_bones = r.i32()?;
    let supple = r.count(10000)?;
    let muscle = r.count(10000)?;
    r.bytes(52)?;
    let unsupported = SkinOmissions {
        files: usize::from(morph_skin + morph_rigid + supple + muscle != 0),
        morph_skin,
        morph_rigid,
        supple,
        muscle,
    };
    Ok((
        r,
        Header {
            binary,
            version,
            skin,
            rigid,
            tex_count,
            material_count,
            bone_count,
            unsupported,
        },
    ))
}

impl Skin {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let (
            mut r,
            Header {
                binary,
                version,
                skin,
                rigid,
                tex_count,
                material_count,
                bone_count,
                unsupported,
            },
        ) = skin_header(bytes)?;
        if unsupported.files != 0 {
            return Err(format!(
                "morph, supple and muscle skin meshes are not supported yet ({unsupported})"
            ));
        }
        let mut bones = Vec::new();
        if version >= 9 {
            for _ in 0..bone_count {
                bones.push(r.string()?);
            }
        }
        let mut textures = Vec::new();
        for _ in 0..tex_count {
            textures.push(r.string()?);
        }
        let mut materials = Vec::new();
        for _ in 0..material_count {
            if !binary {
                return Err("text skin materials unsupported".into());
            }
            let name = r.cstring()?;
            let ambient = r.vec()?;
            let diffuse = r.vec()?;
            let emissive = r.vec()?;
            let specular = r.vec()?;
            let power = r.f32()?;
            let two_sided = r.bytes(1)?[0] != 0;
            materials.push(Material {
                name,
                ambient,
                diffuse,
                emissive,
                specular,
                power,
                two_sided,
            });
        }
        let mut meshes = Vec::new();
        for index in 0..skin + rigid {
            let is_rigid = index >= skin;
            let name = r.string()?;
            let rigid_bone = if is_rigid { Some(r.i32()?) } else { None };
            let texture = r.i32()?;
            let material = r.i32()?;
            let nv = r.count(4_000_000)?;
            let ni = r.count(12_000_000)?;
            let bytes_per = if is_rigid { 32 } else { 48 };
            let required = nv
                .checked_mul(bytes_per)
                .and_then(|x| x.checked_add(ni * 2))
                .ok_or("mesh size overflow")?;
            if required > r.data.len() - r.pos {
                return Err("truncated skin mesh".into());
            }
            let mut vertices = Vec::with_capacity(nv);
            for _ in 0..nv {
                let position = r.vec()?;
                let (weights, joints) = if is_rigid {
                    ([1., 0., 0., 0.], [0; 4])
                } else {
                    let w = [r.weight()?, r.weight()?, r.weight()?];
                    let j = r.bytes(4)?.try_into().unwrap();
                    ([w[0], w[1], w[2], 1. - w[0] - w[1] - w[2]], j)
                };
                let normal = r.vec()?;
                let uv = r.vec()?;
                vertices.push(Vertex {
                    position,
                    weights,
                    joints,
                    normal,
                    uv,
                });
            }
            let mut indices = Vec::with_capacity(ni);
            for _ in 0..ni {
                let i = u16::from_le_bytes(r.bytes(2)?.try_into().unwrap());
                if i as usize >= nv {
                    return Err("skin index outside vertex array".into());
                }
                indices.push(i);
            }
            if version == 100 || version == 101 {
                r.bytes(nv.checked_mul(16).ok_or("tangent size overflow")?)?;
            }
            if texture < -1
                || texture >= tex_count as i32
                || material < -1
                || material >= material_count as i32
            {
                return Err("skin texture/material index invalid".into());
            }
            meshes.push(Mesh {
                name,
                rigid_bone,
                texture,
                material,
                vertices,
                indices,
            });
        }
        if r.pos != r.data.len() {
            return Err(format!("unparsed skin bytes: {}", r.data.len() - r.pos));
        }
        Ok(Self {
            version,
            bones,
            textures,
            materials,
            meshes,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn put(b: &mut Vec<u8>, x: u32) {
        b.extend_from_slice(&x.to_le_bytes())
    }
    fn string(b: &mut Vec<u8>, s: &str) {
        put(b, s.len() as u32);
        b.extend_from_slice(s.as_bytes());
    }
    fn sample() -> Vec<u8> {
        let mut b = b"MOXBIKSA".to_vec();
        for x in [9, 1, 1, 0, 0, 1, 1, 1, 0, 1, 0, 0] {
            put(&mut b, x)
        }
        b.extend_from_slice(&[0; 52]);
        string(&mut b, "bone");
        string(&mut b, "a.dds");
        b.extend_from_slice(b"MATERIAL: a\0");
        for _ in 0..17 {
            put(&mut b, 1f32.to_bits())
        }
        b.push(1);
        for rigid in [false, true] {
            string(&mut b, "mesh");
            if rigid {
                put(&mut b, 0)
            }
            for x in [0, 0, 1, 3] {
                put(&mut b, x)
            }
            for x in [1., 2., 3.] {
                put(&mut b, f32::to_bits(x));
            }
            if !rigid {
                for x in [0.25, 0.5, 0.0] {
                    put(&mut b, f32::to_bits(x));
                }
                b.extend_from_slice(&[1, 2, 3, 4]);
            }
            for x in [0., 1., 0., 0.5, 0.5] {
                put(&mut b, f32::to_bits(x));
            }
            for _ in 0..3 {
                b.extend_from_slice(&0u16.to_le_bytes());
            }
        }
        b
    }
    #[test]
    fn reads_skin_rigid_material_and_weights() {
        let b = sample();
        let x = Skin::parse(&b).unwrap();
        assert_eq!(x.version, 9);
        assert_eq!(x.bones, ["bone"]);
        assert_eq!(x.textures, ["a.dds"]);
        assert!(x.materials[0].two_sided);
        assert_eq!(x.meshes[0].vertices[0].weights, [0.25, 0.5, 0., 0.25]);
        assert_eq!(x.meshes[0].vertices[0].joints, [1, 2, 3, 4]);
        assert_eq!(x.meshes[1].rigid_bone, Some(0));
    }
    #[test]
    fn rejects_truncation_and_unsupported() {
        let b = sample();
        for n in 0..b.len() {
            assert!(Skin::parse(&b[..n]).is_err(), "n={n}");
        }
        let mut x = b.clone();
        x[20..24].copy_from_slice(&1u32.to_le_bytes());
        assert!(Skin::parse(&x).is_err());
    }
    #[test]
    fn preserves_nonfinite_weights_but_still_rejects_nonfinite_positions() {
        let mut b = sample();
        let position = b
            .windows(12)
            .position(|window| {
                window
                    == [1f32.to_bits(), 2f32.to_bits(), 3f32.to_bits()]
                        .map(u32::to_le_bytes)
                        .concat()
            })
            .unwrap();
        let weights = position + 12;
        b[weights..weights + 4].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
        let skin = Skin::parse(&b).unwrap();
        let parsed = skin.meshes[0].vertices[0].weights;
        assert!(parsed[0].is_nan());
        assert!(parsed[3].is_nan());
        assert_eq!([parsed[1], parsed[2]], [0.5, 0.0]);
        b[position..position + 4].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
        assert!(Skin::parse(&b).is_err());
    }

    #[test]
    fn rejects_unrecognized_trailing_payload() {
        let mut b = sample();
        b.push(0);
        assert!(Skin::parse(&b).unwrap_err().contains("unparsed skin bytes"));
    }
    #[test]
    fn parsed_material_channels_feed_recovered_binding() {
        let mut b = sample();
        let start = b
            .windows(b"MATERIAL: a\0".len())
            .position(|w| w == b"MATERIAL: a\0")
            .unwrap()
            + b"MATERIAL: a\0".len();
        for (i, v) in [
            0.2f32, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 0.01, 0.02, 0.03, 0.04, 0.11, 0.12, 0.13,
            0.14,
        ]
        .into_iter()
        .enumerate()
        {
            b[start + i * 4..start + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        let skin = Skin::parse(&b).unwrap();
        assert_eq!(skin.materials[0].ambient, [0.2, 0.3, 0.4, 0.5]);
        assert_eq!(skin.materials[0].diffuse, [0.6, 0.7, 0.8, 0.9]);
        assert_eq!(skin.materials[0].emissive, [0.01, 0.02, 0.03, 0.04]);
        assert_eq!(skin.materials[0].specular, [0.11, 0.12, 0.13, 0.14]);
        let c = skin.materials[0].light_constants(crate::model_material::Lighting {
            environment_ambient: [0.5; 4],
            additional_ambient: [0.; 4],
            directional_diffuse: [0.5; 4],
            point_ambient: [0.5; 4],
            point_diffuse: [0.5; 4],
            transparency: 0.25,
            point_enabled: true,
        });
        assert!((c.ambient[0] - 0.11).abs() < 1e-6);
        assert_eq!(c.directional_diffuse, [0.3, 0.35, 0.4, 0.75]);
        assert_eq!(c.point_diffuse[3], 0.);
    }
}

#[cfg(test)]
#[test]
fn inventory_distinguishes_unsupported_skin_families_and_deduplicates_paths() {
    let mut bytes = b"MOXBIKSA".to_vec();
    for word in [9u32, 0, 0, 2, 3, 0, 0, 0, 0, 0, 5, 7] {
        bytes.extend(word.to_le_bytes());
    }
    bytes.extend([0; 52]);
    let mut census = UnsupportedSkins::default();
    census.observe("models/asset.ski", &bytes);
    census.observe("Models\\Asset.ski", &bytes);
    census.observe("models/short.ski", &bytes[..bytes.len() - 1]);
    assert_eq!(
        census.totals(),
        SkinOmissions {
            files: 1,
            morph_skin: 2,
            morph_rigid: 3,
            supple: 5,
            muscle: 7
        }
    );
    assert!(Skin::parse(&bytes).unwrap_err().contains("morph 2/3"));
}

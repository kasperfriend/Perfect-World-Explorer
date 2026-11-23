//! Bounds-checked Angelica BON skeleton reader (v6).
//! Layout derived from GPL-2.0-or-later GameForge Angelica importer at
//! 0a7fae8fcba0df7b0daa988db8696a0feaf49f7b. Old BONs with embedded tracks
//! use embedded version-1 bone tracks; palette conventions and integration
//! remain unfinished.
use crate::animation::Reader;
#[derive(Debug, Clone)]
pub struct Bone {
    pub name: String,
    pub flags: u8,
    pub parent: i32,
    pub first_joint: i32,
    pub relative: [f32; 16],
    pub initial: [f32; 16],
    pub children: Vec<i32>,
}
#[derive(Debug, Clone)]
pub struct Joint {
    pub class: u32,
    pub name: String,
    pub parent: i32,
    pub child: i32,
    pub sibling: i32,
}
#[derive(Debug, Clone)]
pub struct Hook {
    pub name: String,
    pub kind: u32,
    pub bone: i32,
    pub matrix: [f32; 16],
}
#[derive(Debug, Clone)]
pub struct Skeleton {
    pub version: u32,
    pub start: i32,
    pub end: i32,
    pub fps: i32,
    pub bones: Vec<Bone>,
    pub joints: Vec<Joint>,
    pub hooks: Vec<Hook>,
    pub embedded_tracks: Vec<crate::animation::BoneTrack>,
}
impl Skeleton {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let b = bytes.strip_prefix(b"MOXB").unwrap_or(bytes);
        let mut r = Reader { b, p: 0 };
        if r.take(4)? != b"EKSA" {
            return Err("invalid BON signature".into());
        }
        let version = r.u32()?;
        if !(1..=6).contains(&version) {
            return Err(format!("unsupported BON version {version}"));
        }
        let nb = r.count(10000)?;
        let nj = r.count(10000)?;
        let nh = r.count(10000)?;
        let start = r.i32()?;
        let end = r.i32()?;
        let fps = r.i32()?;
        r.take(64)?;
        let mut bones = Vec::with_capacity(nb);
        for _ in 0..nb {
            let name = r.string()?;
            let flags = r.take(4)?[0];
            let parent = r.i32()?;
            let first_joint = r.i32()?;
            let nc = r.count(10000)?;
            let relative = r.vec()?;
            let initial = r.vec()?;
            let mut children = Vec::with_capacity(nc);
            for _ in 0..nc {
                children.push(r.i32()?)
            }
            bones.push(Bone {
                name,
                flags,
                parent,
                first_joint,
                relative,
                initial,
                children,
            });
        }
        for (i, b) in bones.iter().enumerate() {
            if b.parent < -1
                || b.parent >= nb as i32
                || b.parent == i as i32
                || b.children.iter().any(|&c| c < 0 || c >= nb as i32)
            {
                return Err("invalid bone hierarchy".into());
            }
            let mut p = b.parent;
            for step in 0..=nb {
                if p < 0 {
                    break;
                }
                if step == nb {
                    return Err("cyclic bone hierarchy".into());
                }
                p = bones[p as usize].parent;
            }
        }
        let mut classes = Vec::with_capacity(nj);
        for _ in 0..nj {
            let c = r.u32()?;
            if ![12, 15, 16, 17].contains(&c) {
                return Err("unsupported joint class".into());
            }
            classes.push(c)
        }
        let mut joints = Vec::with_capacity(nj);
        let mut embedded_tracks = Vec::with_capacity(if version < 6 { nj } else { 0 });
        for (index, class) in classes.into_iter().enumerate() {
            joints.push(Joint {
                class,
                name: r.string()?,
                parent: r.i32()?,
                child: r.i32()?,
                sibling: r.i32()?,
            });
            if version < 6 {
                embedded_tracks.push(crate::animation::BoneTrack::read_embedded(
                    &mut r,
                    index as i32,
                )?);
            }
        }
        let mut hooks = Vec::with_capacity(nh);
        for _ in 0..nh {
            hooks.push(Hook {
                name: r.string()?,
                kind: r.u32()?,
                bone: r.i32()?,
                matrix: r.vec()?,
            });
        }
        Ok(Self {
            version,
            start,
            end,
            fps,
            bones,
            joints,
            hooks,
            embedded_tracks,
        })
    }
}

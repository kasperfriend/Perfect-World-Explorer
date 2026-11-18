//! Partial Angelica .stck animation loading and keyframe sampling.
//! Format informed by GameForge 3D Studio (GPL-2.0-or-later),
//! `angelica_anim.cc` at commit 0a7fae8fcba0df7b0daa988db8696a0feaf49f7b.
//! Imported track times, compressed quaternions and v1 segments must still
//! be compared with the retail runtime before claiming animation parity.
use glam::{Quat, Vec3};
use std::collections::HashSet;

pub(crate) struct Reader<'a> {
    pub b: &'a [u8],
    pub p: usize,
}
impl<'a> Reader<'a> {
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.p.checked_add(n).ok_or("length overflow")?;
        let s = self.b.get(self.p..end).ok_or("truncated animation data")?;
        self.p = end;
        Ok(s)
    }
    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    pub fn f32(&mut self) -> Result<f32, String> {
        let v = f32::from_bits(self.u32()?);
        if !v.is_finite() {
            return Err("nonfinite animation data".into());
        }
        Ok(v)
    }
    pub fn string(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        if n > 4096 {
            return Err("oversized animation string".into());
        }
        let b = self.take(n)?;
        let (s, _, bad) = encoding_rs::GBK.decode(b);
        if bad {
            return Err("invalid GBK animation string".into());
        }
        Ok(s.trim_end_matches('\0').to_string())
    }
    pub fn count(&mut self, max: usize) -> Result<usize, String> {
        let v = self.i32()?;
        if v < 0 || v as usize > max {
            Err("invalid animation count".into())
        } else {
            Ok(v as usize)
        }
    }
    pub fn vec<const N: usize>(&mut self) -> Result<[f32; N], String> {
        let mut a = [0.; N];
        for x in &mut a {
            *x = self.f32()?;
        }
        Ok(a)
    }
}
#[derive(Clone, Debug)]
pub struct KeyTrack<const N: usize> {
    pub fps: i32,
    pub keys: Vec<[f32; N]>,
    pub frames: Vec<f32>,
    pub length_ms: i32,
    pub compression: i32,
}
impl<const N: usize> KeyTrack<N> {
    fn read(r: &mut Reader<'_>, version: u32, quaternion: bool) -> Result<Self, String> {
        let n = r.count(2_000_000)?;
        let segs = r.count(2_000_000)?;
        let fps = r.i32()?;
        if n > 0 && fps <= 0 {
            return Err("invalid keyframe rate".into());
        }
        let format = if version >= 2 && quaternion {
            r.i32()?
        } else {
            0
        };
        if format != 0 && format != 1 {
            return Err("unsupported quaternion format".into());
        }
        let mut keys = Vec::with_capacity(n);
        for _ in 0..n {
            let mut a = [0.; N];
            let comps = if format == 1 { 3 } else { N };
            for x in a.iter_mut().take(comps) {
                *x = r.f32()?
            }
            if format == 1 {
                if N != 4 {
                    return Err("invalid compressed quaternion".into());
                }
                a[3] = (1. - a[0] * a[0] - a[1] * a[1] - a[2] * a[2])
                    .max(0.)
                    .sqrt();
            }
            keys.push(a)
        }
        let mut frames = Vec::with_capacity(n);
        let mut length = 0;
        let mut compression = 0;
        if version == 1 {
            let mut ordered = Vec::with_capacity(n);
            let mut f = Vec::with_capacity(n);
            for _ in 0..segs {
                let start = r.i32()?;
                let end = r.i32()?;
                let first = r.i32()?;
                let last = r.i32()?;
                if start < 0 || end < start || first < 0 || last < first || first as usize >= n {
                    return Err(format!(
                        "invalid track segment {start}..{end} keys {first}..{last}/{n}"
                    ));
                }
                let sf = start as f32 * fps as f32 * 0.001;
                let ef = end as f32 * fps as f32 * 0.001;
                let mut add = |idx: usize, frame: f32| {
                    if f.last().map_or(true, |&prev| frame > prev) {
                        ordered.push(keys[idx]);
                        f.push(frame);
                    }
                };
                if first == last {
                    add(first as usize, sf);
                    let tail = ef.ceil() - 1.;
                    if tail > sf {
                        add(first as usize, tail);
                    }
                } else {
                    for k in first..=last.min((n - 1) as i32) {
                        add(k as usize, sf + (k - first) as f32);
                    }
                }
            }
            if !ordered.is_empty() {
                keys = ordered;
                frames = f
            } else {
                frames.extend((0..n).map(|i| i as f32));
            }
        } else {
            length = r.i32()?;
            compression = r.i32()?;
            if length < 0 || !(0..=2).contains(&compression) {
                return Err("invalid track compression".into());
            }
            if compression == 0 {
                frames.extend((0..n).map(|i| i as f32))
            } else {
                for _ in 0..n {
                    frames.push(u16::from_le_bytes(r.take(2)?.try_into().unwrap()) as f32)
                }
            }
        }
        if frames.windows(2).any(|p| p[0] >= p[1]) {
            return Err("non-increasing track key times".into());
        }
        Ok(Self {
            fps,
            keys,
            frames,
            length_ms: length,
            compression,
        })
    }
    fn interval(&self, seconds: f32) -> Option<(usize, usize, f32)> {
        if self.keys.is_empty() || !seconds.is_finite() {
            return None;
        }
        let frame = (seconds.max(0.) * self.fps as f32).max(0.);
        let i = self.frames.partition_point(|x| *x <= frame);
        if i == 0 {
            return Some((0, 0, 0.));
        }
        if i == self.frames.len() {
            let j = i - 1;
            return Some((j, j, 0.));
        }
        let a = i - 1;
        Some((
            a,
            i,
            (frame - self.frames[a]) / (self.frames[i] - self.frames[a]),
        ))
    }
}
impl KeyTrack<3> {
    pub fn sample(&self, seconds: f32) -> Option<[f32; 3]> {
        let (a, b, t) = self.interval(seconds)?;
        Some(
            Vec3::from_array(self.keys[a])
                .lerp(Vec3::from_array(self.keys[b]), t)
                .to_array(),
        )
    }
}
impl KeyTrack<4> {
    pub fn sample(&self, seconds: f32) -> Option<[f32; 4]> {
        let (a, b, t) = self.interval(seconds)?;
        let q = |i| Quat::from_array(self.keys[i]).normalize();
        let x = q(a);
        let y = q(b);
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        Some(x.slerp(y, t).normalize().to_array())
    }
}
#[derive(Clone, Debug)]
pub struct BoneTrack {
    pub id: i32,
    pub position: KeyTrack<3>,
    pub rotation: KeyTrack<4>,
}
impl BoneTrack {
    pub(crate) fn read_embedded(reader: &mut Reader<'_>, id: i32) -> Result<Self, String> {
        Ok(Self {
            id,
            position: KeyTrack::read(reader, 1, false)?,
            rotation: KeyTrack::read(reader, 1, true)?,
        })
    }
}
#[derive(Clone, Debug)]
pub struct TrackSet {
    pub version: u32,
    pub start: i32,
    pub end: i32,
    pub fps: i32,
    pub tracks: Vec<BoneTrack>,
}
impl TrackSet {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let bytes = bytes.strip_prefix(b"MOXB").unwrap_or(bytes);
        let mut r = Reader { b: bytes, p: 0 };
        if r.take(4)? != b"KCTS" {
            return Err("invalid STCK signature".into());
        }
        let version = r.u32()?;
        if !(1..=2).contains(&version) {
            return Err("unsupported STCK version".into());
        }
        let n = r.count(100_000)?;
        let start = r.i32()?;
        let end = r.i32()?;
        let fps = r.i32()?;
        if fps <= 0 || end < start {
            return Err("invalid animation range".into());
        }
        let mut seen = HashSet::with_capacity(n.min(4096));
        let mut tracks = Vec::with_capacity(n);
        for _ in 0..n {
            let id = r.i32()?;
            if !seen.insert(id) {
                return Err("duplicate animation track".into());
            }
            let position = KeyTrack::read(&mut r, version, false)?;
            let rotation = KeyTrack::read(&mut r, version, true)?;
            tracks.push(BoneTrack {
                id,
                position,
                rotation,
            });
        }
        if r.p != bytes.len() {
            return Err(format!(
                "unsupported STCK trailing data: {} bytes",
                bytes.len() - r.p
            ));
        }
        Ok(Self {
            version,
            start,
            end,
            fps,
            tracks,
        })
    }
    pub fn validate_joint_count(&self, count: usize) -> Result<(), String> {
        if self
            .tracks
            .iter()
            .any(|t| t.id < 0 || t.id as usize >= count)
        {
            return Err("animation track references an invalid skeleton joint".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn word(b: &mut Vec<u8>, v: u32) {
        b.extend_from_slice(&v.to_le_bytes());
    }
    fn f(b: &mut Vec<u8>, v: f32) {
        word(b, v.to_bits())
    }
    #[test]
    fn parses_and_samples_version_1_tracks() {
        let mut b = b"MOXBKCTS".to_vec();
        for v in [1, 1, 0, 10, 10, 0] {
            word(&mut b, v)
        }
        for v in [2, 1, 10] {
            word(&mut b, v)
        }
        for v in [0., 0., 0., 10., 0., 0.] {
            f(&mut b, v)
        }
        for v in [0, 200, 0, 1] {
            word(&mut b, v)
        }
        for v in [2, 1, 10] {
            word(&mut b, v)
        }
        for v in [0., 0., 0., 1., 0., 0., 1., 0.] {
            f(&mut b, v)
        }
        for v in [0, 200, 0, 1] {
            word(&mut b, v)
        }
        let t = TrackSet::parse(&b).unwrap();
        assert!(t.validate_joint_count(1).is_ok());
        assert!(t.validate_joint_count(0).is_err());
        let mut negative = t.clone();
        negative.tracks[0].id = -1;
        assert!(negative.validate_joint_count(1).is_err());
        let mut trailing = b.clone();
        trailing.push(0);
        assert!(TrackSet::parse(&trailing).is_err());
        let position = t.tracks[0].position.sample(0.05).unwrap();
        assert!((position[0] - 5.).abs() < 1e-5);
        let rot = t.tracks[0].rotation.sample(0.05).unwrap();
        assert!((rot[2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
        for n in 0..b.len() {
            assert!(TrackSet::parse(&b[..n]).is_err(), "{n}");
        }
    }
    #[test]
    fn compressed_v2_quaternions_and_key_ids() {
        let mut b = b"MOXBKCTS".to_vec();
        for v in [2, 1, 0, 10, 10, 0] {
            word(&mut b, v)
        }
        for v in [2, 0, 10] {
            word(&mut b, v)
        }
        for v in [0., 0., 0., 10., 0., 0.] {
            f(&mut b, v)
        }
        for v in [1000, 1] {
            word(&mut b, v)
        }
        for x in [0u16, 10] {
            b.extend_from_slice(&x.to_le_bytes());
        }
        for v in [2, 0, 10, 1] {
            word(&mut b, v)
        }
        for v in [0., 0., 0., 0., 0., 0.6] {
            f(&mut b, v)
        }
        for v in [1000, 1] {
            word(&mut b, v)
        }
        for x in [0u16, 10] {
            b.extend_from_slice(&x.to_le_bytes());
        }
        let t = TrackSet::parse(&b).unwrap();
        let pos = t.tracks[0].position.sample(0.5).unwrap();
        assert!((pos[0] - 5.).abs() < 1e-5);
        let q = t.tracks[0].rotation.sample(1.).unwrap();
        assert!((q[2] - 0.6).abs() < 1e-5 && (q[3] - 0.8).abs() < 1e-5);
    }
}

//! Native static 3D GFX decals (element 100).
//!
//! This is deliberately a narrow renderer, not a general GFX approximation.
//! It accepts roots whose every element is a non-ground, non-dummy, single-
//! keypoint infinite 3D decal with no controllers or texture animation. Those
//! files execute exactly: A3DDecalEx doubles the authored dimensions, applies
//! the keypoint/parent transforms, modulates texture by packed vertex colour,
//! and submits through the GFX no-depth-write render slot. Other GFX element
//! families and animated layouts remain explicit errors for the caller to
//! report, never static stand-ins.

use crate::ecmodel;
use glam::{Mat4, Quat, Vec3};

#[derive(Clone, Debug)]
pub struct Root {
    default_scale: f32,
    default_alpha: f32,
    pub decals: Vec<Decal>,
}

#[derive(Clone, Debug)]
pub struct Decal {
    pub name: String,
    pub texture: String,
    pub source_blend: u32,
    pub destination_blend: u32,
    pub depth_test: bool,
    width: [f32; 2],
    height: [f32; 2],
    no_width_scale: bool,
    no_height_scale: bool,
    reverse_u: bool,
    reverse_v: bool,
    exchange_uv: bool,
    key: KeyPoint,
}

#[derive(Clone, Debug)]
struct KeyPoint {
    position: Vec3,
    color: u32,
    scale: f32,
    direction: Quat,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Quad {
    pub texture: String,
    pub source_blend: u32,
    pub destination_blend: u32,
    pub depth_test: bool,
    pub vertices: [Vertex; 4],
}

fn number<T: std::str::FromStr>(key: &str, value: &str) -> Result<T, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("invalid GFX {key}: {value}"))
}

fn float(key: &str, value: &str) -> Result<f32, String> {
    let value: f32 = number(key, value)?;
    if !value.is_finite() {
        return Err(format!("non-finite GFX {key}"));
    }
    Ok(value)
}

fn packed(value: &str) -> Result<u32, String> {
    let value: i64 = number("packed colour", value)?;
    if !(-2_147_483_648..=4_294_967_295).contains(&value) {
        return Err("invalid GFX packed colour".into());
    }
    Ok(value as u32)
}

fn boolean(key: &str, value: &str) -> Result<bool, String> {
    match number::<i32>(key, value)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(format!("invalid GFX boolean {key}: {value}")),
    }
}

fn vector<const N: usize>(key: &str, value: &str) -> Result<[f32; N], String> {
    let values = value
        .split(',')
        .map(|part| float(key, part))
        .collect::<Result<Vec<_>, _>>()?;
    values
        .try_into()
        .map_err(|_| format!("invalid GFX {key} component count"))
}

fn one<'a>(fields: &'a [(String, String)], key: &str) -> Result<Option<&'a str>, String> {
    let mut found = fields
        .iter()
        .filter(|(name, _)| name == key)
        .map(|(_, value)| value.as_str());
    let value = found.next();
    if found.next().is_some() {
        return Err(format!("duplicate GFX {key}"));
    }
    Ok(value)
}

fn required<'a>(fields: &'a [(String, String)], key: &str) -> Result<&'a str, String> {
    one(fields, key)?.ok_or_else(|| format!("missing GFX {key}"))
}

fn optional_float(fields: &[(String, String)], key: &str, default: f32) -> Result<f32, String> {
    one(fields, key)?.map_or(Ok(default), |value| float(key, value))
}

fn zero_flag(fields: &[(String, String)], key: &str) -> Result<(), String> {
    if one(fields, key)?.is_some_and(|value| value.trim() != "0") {
        return Err(format!("unsupported static 3D decal {key}"));
    }
    Ok(())
}

fn allowed(fields: &[(String, String)], allowed: &[&str]) -> Result<(), String> {
    if let Some((key, _)) = fields
        .iter()
        .find(|(key, _)| !allowed.contains(&key.as_str()))
    {
        return Err(format!("unrecognized static 3D decal field {key}"));
    }
    Ok(())
}

fn root_path(texture: &str) -> Result<String, String> {
    let path = ecmodel::normalize(texture);
    if path.is_empty()
        || path
            .split('/')
            .any(|part| part == "." || part == ".." || part.contains(':'))
    {
        return Err(format!("unsafe GFX texture path: {texture}"));
    }
    Ok(format!("gfx/textures/{path}"))
}

impl Root {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lines = text.lines();
        let version: u32 = number(
            "version",
            lines
                .next()
                .and_then(|line| line.trim().strip_prefix("MOXTVersion:"))
                .ok_or("expected MOXT text GFX")?,
        )?;
        if version == 0 || version > 79 {
            return Err(format!("unverified static decal GFX version {version}"));
        }
        let mut header = Vec::<(String, String)>::new();
        let mut elements = Vec::<(u32, Vec<(String, String)>)>::new();
        let mut declared = None;
        for line in lines {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "GFXELEMENTCOUNT" => {
                    if declared.replace(number::<usize>(key, value)?).is_some() {
                        return Err("duplicate GFX element count".into());
                    }
                }
                "GFXELEMENTID" => elements.push((number(key, value)?, Vec::new())),
                _ => {
                    let target = if let Some((_, fields)) = elements.last_mut() {
                        fields
                    } else {
                        &mut header
                    };
                    target.push((key.into(), value.into()));
                }
            }
        }
        if declared != Some(elements.len()) {
            return Err("GFX element count mismatch".into());
        }
        if elements.is_empty() {
            return Err("static decal root has no elements".into());
        }
        allowed(
            &header,
            &[
                "DedaultScale",
                "PlaySpeed",
                "DefaultAlpha",
                "Raytrace",
                "FaceToViewer",
                "FadeByDist",
                "FadeStart",
                "FadeEnd",
                "Vec",
                "UseAABB",
                "ShakeCam",
                "NoChangeDir",
                "2DRender",
                "2DBackLayer",
                "PhysExist",
            ],
        )?;
        for key in [
            "Raytrace",
            "FaceToViewer",
            "FadeByDist",
            "ShakeCam",
            "NoChangeDir",
            "2DRender",
            "2DBackLayer",
            "PhysExist",
        ] {
            zero_flag(&header, key)?;
        }
        let default_scale = optional_float(&header, "DedaultScale", 1.)?;
        let default_alpha = optional_float(&header, "DefaultAlpha", 1.)?;
        let play_speed = optional_float(&header, "PlaySpeed", 1.)?;
        if default_alpha < 0. || play_speed < 0. {
            return Err("negative static decal root alpha/speed".into());
        }
        let mut decals = Vec::with_capacity(elements.len());
        for (id, fields) in elements {
            if id != 100 {
                return Err(format!(
                    "unsupported GFX element {id}; static renderer accepts only element 100"
                ));
            }
            decals.push(Decal::parse(version, &fields)?);
        }
        Ok(Self {
            default_scale,
            default_alpha,
            decals,
        })
    }

    /// Evaluate all visible quads for an already resolved event parent. Native
    /// SetParentTM normalizes each parent basis vector, then applies root/event
    /// scale; it does not inherit the model's non-uniform scale.
    pub fn quads(
        &self,
        parent: Mat4,
        event_scale: f32,
        event_alpha: f32,
    ) -> Result<Vec<Quad>, String> {
        if !parent.is_finite()
            || !event_scale.is_finite()
            || !event_alpha.is_finite()
            || event_alpha < 0.
        {
            return Err("invalid static decal event parent/scale/alpha".into());
        }
        let actual_scale = self.default_scale * event_scale;
        let actual_alpha = self.default_alpha * event_alpha;
        if !actual_scale.is_finite() || !actual_alpha.is_finite() {
            return Err("non-finite static decal actual scale/alpha".into());
        }
        let normalize = |axis: glam::Vec4| -> Result<Vec3, String> {
            let axis = axis.truncate();
            let length = axis.length();
            if !length.is_finite() || length <= 1e-12 {
                return Err("degenerate static decal parent basis".into());
            }
            Ok(axis / length)
        };
        let normalized = Mat4::from_cols(
            normalize(parent.x_axis)?.extend(0.),
            normalize(parent.y_axis)?.extend(0.),
            normalize(parent.z_axis)?.extend(0.),
            parent.w_axis,
        );
        let parent = normalized * Mat4::from_scale(Vec3::splat(actual_scale));
        self.decals
            .iter()
            .filter_map(|decal| match decal.quad(parent, actual_alpha) {
                Ok(Some(quad)) => Some(Ok(quad)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }
}

impl Decal {
    fn parse(version: u32, fields: &[(String, String)]) -> Result<Self, String> {
        let split = fields
            .iter()
            .position(|(key, _)| key == "KEYPOINTCOUNT")
            .ok_or("missing static decal keypoint count")?;
        let (base, keypoint) = fields.split_at(split);
        allowed(
            base,
            &[
                "Name",
                "SrcBlend",
                "DestBlend",
                "RepeatCount",
                "RepeatDelay",
                "TexFile",
                "BindEle",
                "ZEnable",
                "MatchGrnd",
                "GroundHeight",
                "TexRow",
                "TexCol",
                "TexInterval",
                "Priority",
                "IsDummy",
                "DummyEle",
                "Warp",
                "TileMode",
                "TexSpeed",
                "UReverse",
                "VReverse",
                "UVExchg",
                "RenderLayer",
                "NoDownSample",
                "ResetLoopEnd",
                "TexAnimMaxTime",
                "Width",
                "Height",
                "RotFromView",
                "GrndNormOnly",
                "NoScale",
                "OrgPt",
                "ZOffset",
                "MatchSurface",
                "SurfaceUseParentDir",
                "MaxExtent",
                "YawEffect",
                "StartTime",
            ],
        )?;
        if required(base, "RepeatCount")? != "1"
            || required(base, "RepeatDelay")? != "0"
            || required(base, "TexRow")? != "1"
            || required(base, "TexCol")? != "1"
            || required(base, "StartTime")? != "0"
        {
            return Err("animated/repeating static decal layout is unsupported".into());
        }
        for key in [
            "MatchGrnd",
            "GroundHeight",
            "IsDummy",
            "Warp",
            "TileMode",
            "RenderLayer",
            "NoDownSample",
            "TexAnimMaxTime",
            "RotFromView",
            "GrndNormOnly",
            "MatchSurface",
            "SurfaceUseParentDir",
            "YawEffect",
        ] {
            zero_flag(base, key)?;
        }
        for key in ["BindEle", "DummyEle"] {
            if !required(base, key)?.is_empty() {
                return Err(format!("unsupported static decal {key} binding"));
            }
        }
        for value in base
            .iter()
            .filter(|(key, _)| key == "TexSpeed")
            .map(|(_, value)| value)
        {
            if float("TexSpeed", value)? != 0. {
                return Err("animated static decal UV offsets are unsupported".into());
            }
        }
        let depth_test = version < 76 || boolean("ZEnable", required(base, "ZEnable")?)?;
        let no_scale = base
            .iter()
            .filter(|(key, _)| key == "NoScale")
            .map(|(_, value)| boolean("NoScale", value))
            .collect::<Result<Vec<_>, _>>()?;
        if !no_scale.is_empty() && no_scale.len() != 2 {
            return Err("static decal NoScale requires width and height".into());
        }
        let origin = base
            .iter()
            .filter(|(key, _)| key == "OrgPt")
            .map(|(_, value)| float("OrgPt", value))
            .collect::<Result<Vec<_>, _>>()?;
        if !origin.is_empty() && origin.len() != 2 {
            return Err("static decal OrgPt requires width and height".into());
        }
        let width = float("Width", required(base, "Width")?)?;
        let height = float("Height", required(base, "Height")?)?;
        if width < 0. || height < 0. {
            return Err("negative static decal dimensions".into());
        }
        let origin = [
            origin.first().copied().unwrap_or(0.5),
            origin.get(1).copied().unwrap_or(0.5),
        ];
        let width1 = width * 2. * origin[0];
        let height1 = height * 2. * origin[1];
        if let Some(value) = one(base, "ZOffset")? {
            if float("ZOffset", value)? != 0. {
                return Err("nonzero static 3D decal ZOffset is unsupported".into());
            }
        }
        let key = KeyPoint::parse(keypoint)?;
        Ok(Self {
            name: required(base, "Name")?.into(),
            texture: root_path(required(base, "TexFile")?)?,
            source_blend: number("SrcBlend", required(base, "SrcBlend")?)?,
            destination_blend: number("DestBlend", required(base, "DestBlend")?)?,
            depth_test,
            width: [width1, width * 2. - width1],
            height: [height1, height * 2. - height1],
            no_width_scale: no_scale.first().copied().unwrap_or(false),
            no_height_scale: no_scale.get(1).copied().unwrap_or(false),
            reverse_u: one(base, "UReverse")?.is_some_and(|v| v == "1"),
            reverse_v: one(base, "VReverse")?.is_some_and(|v| v == "1"),
            exchange_uv: one(base, "UVExchg")?.is_some_and(|v| v == "1"),
            key,
        })
    }

    fn quad(&self, parent: Mat4, actual_alpha: f32) -> Result<Option<Quad>, String> {
        let alpha = (((self.key.color >> 24) & 255) as f32 * actual_alpha).trunc();
        if !(0. ..=255.).contains(&alpha) {
            return Err("static decal alpha conversion is outside one byte".into());
        }
        if alpha < 5. {
            return Ok(None);
        }
        let color = [
            ((self.key.color >> 16) & 255) as f32 / 255.,
            ((self.key.color >> 8) & 255) as f32 / 255.,
            (self.key.color & 255) as f32 / 255.,
            alpha / 255.,
        ];
        let width_scale = if self.no_width_scale {
            1.
        } else {
            self.key.scale
        };
        let height_scale = if self.no_height_scale {
            1.
        } else {
            self.key.scale
        };
        let transform =
            parent * Mat4::from_rotation_translation(self.key.direction, self.key.position);
        let local = [
            [
                -self.width[0] * width_scale,
                self.height[1] * height_scale,
                0.,
            ],
            [
                self.width[1] * width_scale,
                self.height[1] * height_scale,
                0.,
            ],
            [
                -self.width[0] * width_scale,
                -self.height[0] * height_scale,
                0.,
            ],
            [
                self.width[1] * width_scale,
                -self.height[0] * height_scale,
                0.,
            ],
        ];
        let mut uv = [[0., 0.], [1., 0.], [0., 1.], [1., 1.]];
        if self.reverse_u {
            uv[0][0] = 1.;
            uv[1][0] = 0.;
            uv[2][0] = 1.;
            uv[3][0] = 0.;
        }
        if self.reverse_v {
            uv[0][1] = 1.;
            uv[1][1] = 1.;
            uv[2][1] = 0.;
            uv[3][1] = 0.;
        }
        if self.exchange_uv {
            for uv in &mut uv {
                uv.swap(0, 1);
            }
        }
        let vertices: [Vertex; 4] = std::array::from_fn(|index| Vertex {
            position: transform
                .transform_point3(Vec3::from(local[index]))
                .to_array(),
            uv: uv[index],
            color,
        });
        if vertices.iter().any(|vertex| {
            vertex
                .position
                .iter()
                .chain(vertex.uv.iter())
                .chain(vertex.color.iter())
                .any(|value| !value.is_finite())
        }) {
            return Err("non-finite evaluated static decal quad".into());
        }
        Ok(Some(Quad {
            texture: self.texture.clone(),
            source_blend: self.source_blend,
            destination_blend: self.destination_blend,
            depth_test: self.depth_test,
            vertices,
        }))
    }
}

impl KeyPoint {
    fn parse(fields: &[(String, String)]) -> Result<Self, String> {
        if fields.first().map(|x| x.1.as_str()) != Some("1") {
            return Err("static decal requires exactly one keypoint".into());
        }
        let fields = &fields[1..];
        let expected = [
            "InterpolateMode",
            "TimeSpan",
            "Position",
            "Color",
            "Scale",
            "Direction",
            "Rad_2D",
            "CtrlMethodCount",
        ];
        if fields.len() != expected.len()
            || fields
                .iter()
                .zip(expected)
                .any(|((key, _), expected)| key != expected)
        {
            return Err("unrecognized static decal keypoint layout".into());
        }
        if fields[0].1 != "0"
            || fields[1].1 != "-1"
            || fields[6].1 != "0.000000" && fields[6].1 != "0"
            || fields[7].1 != "0"
        {
            return Err("animated/controller static decal keypoint is unsupported".into());
        }
        let direction = vector::<4>("Direction", &fields[5].1)?;
        let direction = Quat::from_xyzw(direction[0], direction[1], direction[2], direction[3]);
        if direction.length_squared() <= 1e-12 {
            return Err("degenerate static decal keypoint direction".into());
        }
        Ok(Self {
            position: Vec3::from(vector::<3>("Position", &fields[2].1)?),
            color: packed(&fields[3].1)?,
            scale: float("Scale", &fields[4].1)?,
            direction: direction.normalize(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        "MOXTVersion: 33\nDedaultScale: 2\nPlaySpeed: 1\nDefaultAlpha: 0.5\nRaytrace: 0\nFadeByDist: 0\nShakeCam: 0\nGFXELEMENTCOUNT: 1\nGFXELEMENTID: 100\nName: glow\nSrcBlend: 5\nDestBlend: 2\nRepeatCount: 1\nRepeatDelay: 0\nTexFile: 粒子\\glow.dds\nBindEle: \nZEnable: 0\nMatchGrnd: 0\nTexRow: 1\nTexCol: 1\nTexInterval: 50\nPriority: 2\nIsDummy: 0\nDummyEle: \nWarp: 0\nWidth: 0.25\nHeight: 0.5\nRotFromView: 0\nGrndNormOnly: 0\nStartTime: 0\nKEYPOINTCOUNT: 1\nInterpolateMode: 0\nTimeSpan: -1\nPosition: 1, 2, 3\nColor: -1\nScale: 2\nDirection: 0, 0, 0, 1\nRad_2D: 0\nCtrlMethodCount: 0\n".into()
    }

    #[test]
    fn native_dimensions_parent_scale_colour_and_old_depth_rule() {
        let root = Root::parse(&sample()).unwrap();
        let parent = Mat4::from_translation(Vec3::new(10., 20., 30.))
            * Mat4::from_scale(Vec3::new(3., 4., 5.));
        let quads = root.quads(parent, 3., 1.).unwrap();
        assert_eq!(quads.len(), 1);
        let quad = &quads[0];
        assert_eq!(quad.texture, "gfx/textures/粒子/glow.dds");
        assert!(quad.depth_test); // version < 76 ignores authored ZEnable: 0
        assert_eq!(quad.vertices[0].color, [1., 1., 1., 127. / 255.]);
        assert_eq!(quad.vertices[0].position, [13., 38., 48.]);
        assert_eq!(quad.vertices[3].position, [19., 26., 48.]);
    }

    #[test]
    fn alpha_cutoff_and_unsupported_animation_are_not_static_substitutes() {
        let root =
            Root::parse(&sample().replace("DefaultAlpha: 0.5", "DefaultAlpha: 0.01")).unwrap();
        assert!(root.quads(Mat4::IDENTITY, 1., 1.).unwrap().is_empty());
        for (before, after) in [
            ("CtrlMethodCount: 0", "CtrlMethodCount: 1"),
            ("TexRow: 1", "TexRow: 2"),
            ("RotFromView: 0", "RotFromView: 1"),
            ("GFXELEMENTID: 100", "GFXELEMENTID: 101"),
            ("TimeSpan: -1", "TimeSpan: 100"),
        ] {
            assert!(
                Root::parse(&sample().replace(before, after)).is_err(),
                "{before}"
            );
        }
    }

    #[test]
    fn authentic_static_weapon_decals_cover_the_audited_root_set() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("gfx.pck").exists() {
            crate::pck::corpus_absent("`google_drive/pw142/element`");
            return;
        }
        let mut file = std::fs::File::open(element.join("gfx.pck")).unwrap();
        let entries = crate::pck::directory(&mut file).unwrap();
        let roots = [
            "gfx/人物/通用/武器/武器效果/17品双斧_花.gfx",
            "gfx/人物/通用/武器/武器效果/18品双剑_剑柄.gfx",
            "gfx/人物/通用/武器/武器效果/18品双剑_剑身高光.gfx",
            "gfx/人物/通用/武器/武器效果/紫色剑光长.gfx",
            "gfx/人物/通用/武器/武器效果/绿色剑光.gfx",
            "gfx/人物/通用/武器/武器效果/绿色流动剑光.gfx",
            "gfx/人物/通用/武器/武器效果/蓝色剑光.gfx",
            "gfx/人物/通用/武器/武器效果/蓝色剑光长.gfx",
            "gfx/人物/通用/武器/武器效果/蓝色流动剑光.gfx",
            "gfx/人物/通用/武器/武器效果/黄色剑光.gfx",
        ];
        for path in roots {
            let entry = entries
                .iter()
                .find(|entry| entry.name == path)
                .unwrap_or_else(|| panic!("missing {path}"));
            let bytes = crate::pck::read_entry(&mut file, entry)
                .unwrap_or_else(|error| panic!("{path}: {error}"));
            let (text, _, bad) = encoding_rs::GBK.decode(&bytes);
            assert!(!bad, "{path}: invalid GBK");
            let root = Root::parse(&text).unwrap_or_else(|error| panic!("{path}: {error}"));
            assert!(!root.decals.is_empty());
            for decal in &root.decals {
                assert!(
                    entries.iter().any(|entry| entry.name == decal.texture),
                    "{} missing",
                    decal.texture
                );
            }
            assert!(!root.quads(Mat4::IDENTITY, 1., 1.).unwrap().is_empty());
        }
    }
}

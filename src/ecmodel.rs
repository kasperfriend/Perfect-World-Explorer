//! Client EC model references and the string table in A3D skin model headers.
//! These are dependency parsers, not a skinning or animation renderer.
#[derive(Debug)]
pub struct EcModel {
    pub version: u32,
    pub skin_model_path: String,
    /// Optional in older descriptors; absence is not an invented render default.
    pub original_color: Option<u32>,
    pub source_blend: Option<u32>,
    pub destination_blend: Option<u32>,
    pub default_speed: Option<f32>,
    pub actions: Vec<CombinedAction>,
    pub events: Vec<EcmEvent>,
    /// `AddiSkinPath` entries: the skins this descriptor actually selects.
    ///
    /// This is the authoritative skin list. The `.smd` it points at carries its
    /// own `.ski` references, but those are a *menu*, not a selection: in
    /// `models.pck`, 228 of 247 `.ski` names referenced by `.smd` files do not
    /// exist, while the folder holds the real ones under variant names
    /// (`冬.smd` lists `冬.ski`; the folder has `冬1.ski`..`冬4.ski`, and
    /// `冬1.ecm` names `冬1.SKI` through `AddiSkinPath`). Treating the `.smd`
    /// list as authoritative drops those models entirely.
    pub additional_skins: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderSettings {
    pub original_color: u32,
    pub source_blend: u32,
    pub destination_blend: u32,
    pub default_speed: f32,
}
impl EcModel {
    /// ECWLD scene load: 0x5b3a4f–0x5b3aca first queries the combined
    /// action map, then falls back to the literal 休闲 (0x95a750). A matching
    /// SMD/base name alone is not a scene action. None means playback failed,
    /// not permission to pick the first action or reinterpret the namespace.
    pub fn scene_action(&self, requested: &str) -> Option<usize> {
        let mut fallback = None;
        for (i, a) in self.actions.iter().enumerate() {
            if a.name == requested {
                return Some(i);
            }
            if fallback.is_none() && a.name == "休闲" {
                fallback = Some(i);
            }
        }
        fallback
    }
    /// Static-data constructor: 0x6d453f/0x6d4547 set white and speed 1;
    /// 0x6d46b5/0x6d46bc set SRCALPHA/INVSRCALPHA. Raw optional fields
    /// stay intact so absence and explicit values can still be audited.
    pub fn render_settings(&self) -> RenderSettings {
        RenderSettings {
            original_color: self.original_color.unwrap_or(u32::MAX),
            source_blend: self.source_blend.unwrap_or(5),
            destination_blend: self.destination_blend.unwrap_or(6),
            default_speed: self.default_speed.unwrap_or(1.),
        }
    }
}
#[derive(Debug, Default)]
pub struct CombinedAction {
    pub name: String,
    pub loops: Option<i32>,
    pub speed: Option<f32>,
    pub bases: Vec<BaseAction>,
}
impl CombinedAction {
    /// Retail 0x6f2061–0x6f2079 forces a sole base to loop forever
    /// when its containing combined action loops forever. The loader
    /// initializes both base bounds to one (0x6f2004–0x6f2012).
    /// This is loop-policy normalization, not a playback scheduler.
    pub fn base_loop_bounds(&self, index: usize) -> Option<(i32, i32)> {
        let base = self.bases.get(index)?;
        if self.loops == Some(-1) && self.bases.len() == 1 {
            Some((-1, -1))
        } else {
            Some((base.loop_min.unwrap_or(1), base.loop_max.unwrap_or(1)))
        }
    }
}
#[derive(Debug, Default)]
pub struct BaseAction {
    pub name: String,
    pub start_ms: Option<i32>,
    pub loop_min: Option<i32>,
    pub loop_max: Option<i32>,
}
/// Preserve event fields without claiming to implement their playback semantics.
#[derive(Debug)]
pub struct EcmEvent {
    pub kind: i32,
    pub action: Option<String>,
    pub fields: Vec<(String, String)>,
}
impl EcmEvent {
    pub fn gfx_path(&self) -> Option<String> {
        (self.kind == 100)
            .then(|| self.fields.iter().find(|x| x.0 == "FxFilePath"))
            .flatten()
            .map(|x| normalize(&x.1))
            .filter(|x| !x.is_empty())
    }
}
#[derive(Debug)]
pub struct SkinModel {
    pub version: u32,
    pub skeleton: String,
    pub skins: Vec<String>,
    pub physique: String,
    pub track_directory: String,
    pub actions: Vec<Action>,
    pub hangers: Vec<Hanger>,
    pub properties: Vec<(String, String)>,
}
#[derive(Debug, Clone)]
pub struct Action {
    pub name: String,
    pub start: f32,
    pub end: f32,
    pub fps: Option<i32>,
    pub track_file: String,
    pub joints: Vec<JointAction>,
}
#[derive(Debug, Clone)]
pub struct JointAction {
    pub joint: i32,
    pub start: f32,
    pub end: f32,
    pub loops: i32,
    pub speed: f32,
}
#[derive(Debug)]
pub struct Hanger {
    pub kind: i32,
    pub name: String,
    pub target: String,
    pub on_bone: bool,
    pub file: String,
    pub child_hook: String,
}
impl SkinModel {
    /// External STCK paths are relative to the SMD's own directory, then its
    /// track-directory field; they are not relative to models.pck's root.
    pub fn action_track_path(&self, descriptor_path: &str, action: &Action) -> Option<String> {
        if action.track_file.is_empty() {
            return None;
        }
        let descriptor = normalize(descriptor_path);
        let parent = descriptor.rsplit_once('/').map_or("", |x| x.0);
        Some(normalize(&format!(
            "{parent}/{}/{}",
            self.track_directory, action.track_file
        )))
    }
}
fn gbk(bytes: &[u8]) -> Result<String, String> {
    let (s, _, bad) = encoding_rs::GBK.decode(bytes);
    if bad {
        Err("invalid GBK model path".into())
    } else {
        Ok(s.into_owned())
    }
}
pub fn normalize(path: &str) -> String {
    let trimmed = path.trim();
    let mut out = String::with_capacity(trimmed.len());
    for seg in trimmed.split(['/', '\\']).filter(|s| !s.is_empty()) {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(seg);
    }
    if out.is_ascii() {
        out.make_ascii_lowercase();
        out
    } else {
        out.to_lowercase()
    }
}
pub fn parse_ecm(bytes: &[u8]) -> Result<EcModel, String> {
    let text = gbk(bytes)?;
    let mut version = None;
    let mut path = None;
    let mut original_color = None;
    let mut source_blend = None;
    let mut destination_blend = None;
    let mut default_speed = None;
    let mut declared_actions = None;
    let mut actions = Vec::<CombinedAction>::new();
    let mut base_counts = Vec::<Option<usize>>::new();
    let mut events = Vec::<EcmEvent>::new();
    let mut additional_skins: Vec<String> = Vec::new();
    let mut additional_skin_count: Option<usize> = None;
    let mut header = true;
    let mut in_bases = false;
    let mut in_event = false;
    fn integer<T: std::str::FromStr>(key: &str, value: &str) -> Result<T, String> {
        value
            .parse()
            .map_err(|_| format!("invalid ECM {key}: {value}"))
    }
    fn speed(key: &str, value: &str) -> Result<f32, String> {
        let v: f32 = integer(key, value)?;
        if !v.is_finite() {
            return Err(format!("invalid ECM {key}: {value}"));
        }
        Ok(v)
    }
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "MOXTVersion" => version = Some(integer(key, value)?),
            "SkinModelPath" => path = Some(normalize(value)),
            "OrgColor" if header => {
                original_color =
                    Some(u32::from_str_radix(value, 16).map_err(|_| "invalid ECM OrgColor")?)
            }
            "SrcBlend" if header => source_blend = Some(integer(key, value)?),
            "DestBlend" if header => destination_blend = Some(integer(key, value)?),
            "DefSpeed" if header => default_speed = Some(speed(key, value)?),
            "ComActCount" => declared_actions = Some(integer::<usize>(key, value)?),
            "CombineActName" => {
                if value.is_empty() {
                    return Err("empty ECM combined-action name".into());
                }
                header = false;
                in_bases = false;
                in_event = false;
                actions.push(CombinedAction {
                    name: value.into(),
                    ..Default::default()
                });
                base_counts.push(None);
            }
            "EventType" => {
                header = false;
                in_bases = false;
                in_event = true;
                events.push(EcmEvent {
                    kind: integer(key, value)?,
                    action: actions.last().map(|a| a.name.clone()),
                    fields: Vec::new(),
                });
            }
            "AddiSkinCount" => {
                // Declared count; the paths follow as AddiSkinPath entries. The
                // count is validated after the loop rather than trusted.
                additional_skin_count = Some(integer::<usize>(key, value)?);
            }
            "AddiSkinPath" => {
                if !value.is_empty() {
                    additional_skins.push(value.into());
                }
            }
            _ if in_event && !matches!(key, "ChildCount" | "PhysFileName" | "ECMHookCount") => {
                events
                    .last_mut()
                    .unwrap()
                    .fields
                    .push((key.into(), value.into()));
            }
            "BaseActCount" => {
                let count = integer::<usize>(key, value)?;
                *base_counts
                    .last_mut()
                    .ok_or("ECM base count without combined action")? = Some(count);
                in_bases = true;
            }
            "BaseActName" => {
                if !in_bases || value.is_empty() {
                    return Err("ECM base action outside base list or empty name".into());
                }
                actions
                    .last_mut()
                    .ok_or("ECM base action without combined action")?
                    .bases
                    .push(BaseAction {
                        name: value.into(),
                        ..Default::default()
                    });
            }
            "LoopCount" => {
                let loops = integer(key, value)?;
                if let Some(action) = actions.last_mut() {
                    if in_bases {
                        let base = action
                            .bases
                            .last_mut()
                            .ok_or("ECM loop count before base action")?;
                        base.loop_min = Some(loops);
                        base.loop_max = Some(loops);
                    } else {
                        action.loops = Some(loops);
                    }
                }
            }
            "PlaySpeed" => {
                if let Some(action) = actions.last_mut() {
                    action.speed = Some(speed(key, value)?);
                }
            }
            "ActStartTime" | "LoopMinNum" | "LoopMaxNum" if in_bases => {
                let base = actions
                    .last_mut()
                    .and_then(|a| a.bases.last_mut())
                    .ok_or("ECM base timing without base action")?;
                let v = Some(integer(key, value)?);
                match key {
                    "ActStartTime" => base.start_ms = v,
                    "LoopMinNum" => base.loop_min = v,
                    _ => base.loop_max = v,
                }
            }
            "EventCount" => {
                in_bases = false;
            }
            _ => {}
        }
    }
    if declared_actions.is_some_and(|n| n != actions.len()) {
        return Err("ECM combined-action count mismatch".into());
    }
    for (action, count) in actions.iter().zip(base_counts) {
        if count.is_some_and(|n| n != action.bases.len()) {
            return Err(format!("ECM base-action count mismatch: {}", action.name));
        }
        for base in &action.bases {
            if let (Some(min), Some(max)) = (base.loop_min, base.loop_max) {
                if min >= 0 && max >= 0 && min > max {
                    return Err(format!("ECM inverted loop bounds: {}", base.name));
                }
            }
        }
    }
    Ok(EcModel {
        version: version.ok_or("missing ECM version")?,
        skin_model_path: path
            .filter(|x| !x.is_empty())
            .ok_or("missing ECM skin model")?,
        original_color,
        source_blend,
        destination_blend,
        default_speed,
        actions,
        events,
        additional_skins: {
            // The declared count must agree with the paths actually seen, or the
            // descriptor is malformed and we say so rather than guessing.
            if let Some(n) = additional_skin_count {
                if n != additional_skins.len() {
                    return Err(format!(
                        "ECM AddiSkinCount {n} but {} AddiSkinPath entries",
                        additional_skins.len()
                    ));
                }
            }
            additional_skins
        },
    })
}
/// Parse A3D skin-model descriptors (v1–v9), including animation and attachments.
/// Layout informed by GameForge 3D Studio `angelica_model.cc` GPL-2.0-or-later,
/// commit 0a7fae8fcba0df7b0daa988db8696a0feaf49f7b.
pub fn parse_smd(bytes: &[u8]) -> Result<SkinModel, String> {
    use crate::animation::Reader;
    let body = bytes
        .strip_prefix(b"MOXB")
        .ok_or("unsupported SMD AFile header")?;
    let mut r = Reader { b: body, p: 0 };
    if r.take(4)? != b"DMSA" {
        return Err("invalid SMD header".into());
    }
    let version = r.u32()?;
    if !(1..=9).contains(&version) {
        return Err(format!("unsupported SMD version {version}"));
    }
    let count = r.count(4096)?;
    let action_count = r.count(100000)?;
    let hanger_count = r.count(100000)?;
    let prop_count = r.count(100000)?;
    r.take(60)?;
    let skeleton = r.string()?;
    let mut skins = Vec::with_capacity(count);
    for _ in 0..count {
        skins.push(r.string()?);
    }
    let physique = r.string()?;
    let track_directory = if version >= 8 {
        r.string()?
    } else {
        String::new()
    };
    let mut actions = Vec::with_capacity(action_count);
    for _ in 0..action_count {
        let name = r.string()?;
        let mut joints = Vec::new();
        let (start, end, fps) = if version < 6 {
            let _group = r.i32()?;
            let n = r.count(100000)?;
            joints.reserve((n as usize).min(1024));
            let _flags = r.i32()?;
            for _ in 0..n {
                joints.push(JointAction {
                    joint: r.i32()?,
                    start: r.f32()?,
                    end: r.f32()?,
                    loops: r.i32()?,
                    speed: r.f32()?,
                });
            }
            joints
                .first()
                .map(|j| (j.start, j.end, None))
                .unwrap_or((0., 0., None))
        } else {
            let start = r.f32()?;
            let end = r.f32()?;
            let fps = if version >= 9 { Some(r.i32()?) } else { None };
            (start, end, fps)
        };
        let track_file = if version >= 7 {
            r.string()?
        } else {
            String::new()
        };
        actions.push(Action {
            name,
            start,
            end,
            fps,
            track_file,
            joints,
        });
    }
    let mut hangers = Vec::with_capacity(hanger_count);
    for _ in 0..hanger_count {
        let kind = r.i32()?;
        if kind != 0 && kind != 1 {
            return Err(format!("unsupported SMD hanger type {kind}"));
        }
        let name = r.string()?;
        let target = r.string()?;
        let _record_kind = r.i32()?;
        let on_bone = r.take(4)?[0] != 0;
        let file = r.string()?;
        let child_hook = if kind == 1 {
            r.string()?
        } else {
            String::new()
        };
        hangers.push(Hanger {
            kind,
            name,
            target,
            on_bone,
            file,
            child_hook,
        });
    }
    let mut properties = Vec::with_capacity(prop_count);
    for _ in 0..prop_count {
        properties.push((r.string()?, r.string()?));
    }
    Ok(SkinModel {
        version,
        skeleton,
        skins,
        physique,
        track_directory,
        actions,
        hangers,
        properties,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_the_descriptor_skin_list_the_client_actually_uses() {
        // Values copied from models/宠物/精灵/冬/冬1.ecm: a full pack path with
        // Windows separators and no leading `models/`, which is how the retail
        // descriptors store it.
        // The descriptor is GBK, not UTF-8: `parse_ecm` rejects bytes GBK cannot
        // decode, so the fixture has to be encoded the way the client ships it.
        let src = "MOXTVersion: 21\r\nSkinModelPath: Models\\NPCs\\宠物\\精灵\\冬\\冬.SMD\r\n\
                   AddiSkinCount: 1\r\nAddiSkinPath: NPCs\\宠物\\精灵\\冬\\冬1.SKI\r\n";
        let (bytes, _, unmappable) = encoding_rs::GBK.encode(src);
        assert!(!unmappable, "the fixture must be representable in GBK");
        let x = parse_ecm(&bytes).unwrap();
        assert_eq!(x.additional_skins, ["NPCs\\宠物\\精灵\\冬\\冬1.SKI"]);
    }
    #[test]
    fn a_skin_count_that_disagrees_with_its_paths_is_rejected() {
        let base = "MOXTVersion: 21\r\nSkinModelPath: Models\\a.smd\r\n";
        // Declared 2, one path.
        let src = format!("{base}AddiSkinCount: 2\r\nAddiSkinPath: a\\b.ski\r\n");
        assert!(parse_ecm(src.as_bytes()).is_err());
        // Declared 1, two paths.
        let src = format!(
            "{base}AddiSkinCount: 1\r\nAddiSkinPath: a\\b.ski\r\nAddiSkinPath: a\\c.ski\r\n"
        );
        assert!(parse_ecm(src.as_bytes()).is_err());
        // Matching counts are accepted.
        let src = format!("{base}AddiSkinCount: 1\r\nAddiSkinPath: a\\b.ski\r\n");
        assert_eq!(parse_ecm(src.as_bytes()).unwrap().additional_skins.len(), 1);
    }
    #[test]
    fn descriptors_without_addi_skin_leave_the_list_empty() {
        let src = "MOXTVersion: 21\r\nSkinModelPath: Models\\a.smd\r\n";
        assert!(parse_ecm(src.as_bytes())
            .unwrap()
            .additional_skins
            .is_empty());
    }

    #[test]
    fn path_normalization() {
        assert_eq!(normalize("Models\\\\Stuff\\A.SMD"), "models/stuff/a.smd");
    }
    #[test]
    fn smd_rejects_truncation() {
        assert!(parse_smd(b"MOXBDMSA").is_err());
    }
    #[test]
    fn smd_reads_all_referenced_skins_and_checks_lengths() {
        fn put(b: &mut Vec<u8>, v: u32) {
            b.extend_from_slice(&v.to_le_bytes());
        }
        fn text(b: &mut Vec<u8>, s: &str) {
            put(b, s.len() as u32);
            b.extend_from_slice(s.as_bytes());
        }
        let mut b = b"MOXBDMSA".to_vec();
        put(&mut b, 8);
        put(&mut b, 2);
        b.resize(88, 0);
        text(&mut b, "rig.bon");
        text(&mut b, "body.ski");
        text(&mut b, "extra.ski");
        text(&mut b, "");
        text(&mut b, "");
        let smd = parse_smd(&b).unwrap();
        assert_eq!(smd.skeleton, "rig.bon");
        assert_eq!(smd.skins, ["body.ski", "extra.ski"]);
        for n in 0..b.len() {
            assert!(parse_smd(&b[..n]).is_err(), "{n}");
        }
        b[12..16].copy_from_slice(&4097u32.to_le_bytes());
        assert!(parse_smd(&b).is_err());
    }
    #[test]
    fn parses_action_hanger_and_properties() {
        fn put(b: &mut Vec<u8>, v: u32) {
            b.extend_from_slice(&v.to_le_bytes());
        }
        fn text(b: &mut Vec<u8>, s: &str) {
            put(b, s.len() as u32);
            b.extend_from_slice(s.as_bytes());
        }
        let mut b = b"MOXBDMSA".to_vec();
        for x in [8u32, 1, 1, 1, 1] {
            put(&mut b, x);
        }
        b.resize(88, 0);
        for s in ["rig.bon", "body.ski", "body.phy", "tracks", "idle"] {
            text(&mut b, s);
        }
        put(&mut b, 0f32.to_bits());
        put(&mut b, 30f32.to_bits());
        text(&mut b, "idle.stck");
        put(&mut b, 1);
        text(&mut b, "weapon");
        text(&mut b, "hand");
        put(&mut b, 1);
        put(&mut b, 1);
        text(&mut b, "weapon.ecm");
        text(&mut b, "root");
        text(&mut b, "mode");
        text(&mut b, "visible");
        let m = parse_smd(&b).unwrap();
        assert_eq!(m.track_directory, "tracks");
        assert_eq!(m.actions[0].track_file, "idle.stck");
        assert_eq!(m.actions[0].end, 30.);
        assert_eq!(
            m.action_track_path("Models\\Scene\\Rig.SMD", &m.actions[0])
                .as_deref(),
            Some("models/scene/tracks/idle.stck")
        );
        let mut embedded = m.actions[0].clone();
        embedded.track_file.clear();
        assert!(m.action_track_path("models/rig.smd", &embedded).is_none());
        assert!(m.hangers[0].on_bone);
        assert_eq!(m.hangers[0].child_hook, "root");
        assert_eq!(m.properties[0], ("mode".into(), "visible".into()));
        for n in 0..b.len() {
            assert!(parse_smd(&b[..n]).is_err(), "{n}");
        }
    }
    #[test]
    fn parse_ecm_header() {
        let x = parse_ecm(b"MOXTVersion: 19\r\nSkinModelPath: Models\\Foo.SMD\r\n").unwrap();
        assert_eq!(x.version, 19);
        assert_eq!(x.skin_model_path, "models/foo.smd");
    }
}

#[cfg(test)]
mod ecm_schedule_tests {
    use super::*;
    fn parse(body: &str) -> Result<EcModel, String> {
        parse_ecm(format!("MOXTVersion: 47\nSkinModelPath: Models\\test.smd\n{body}").as_bytes())
    }
    #[test]
    fn scene_load_uses_combined_map_then_idle_not_direct_base_names() {
        let m=parse("ComActCount: 2\nCombineActName: visible\nLoopCount: 1\nBaseActCount: 1\nBaseActName: direct-only\nActStartTime: 0\nLoopCount: 1\nEventCount: 0\nCombineActName: idle\nLoopCount: 1\nBaseActCount: 0\nEventCount: 0\n").unwrap();
        let mut m = m;
        m.actions[1].name = "休闲".into();
        assert_eq!(m.scene_action("visible"), Some(0));
        assert_eq!(m.scene_action("direct-only"), Some(1));
        assert_eq!(m.scene_action("missing"), Some(1));
        let mut m = m;
        m.actions.pop();
        assert_eq!(m.scene_action("direct-only"), None);
    }
    #[test]
    fn maps_combined_name_and_keeps_random_loop_schedule() {
        let m=parse("OrgColor: ff804020\nSrcBlend: 5\nDestBlend: 6\nDefSpeed: 1.25\nComActCount: 1\nCombineActName: visible\nLoopCount: -1\nPlaySpeed: 0.5\nBaseActCount: 2\nBaseActName: local-a\nActStartTime: 0\nLoopMinNum: 3\nLoopMaxNum: 10\nBaseActName: local-b\nActStartTime: 3350\nLoopMinNum: 1\nLoopMaxNum: 1\nEventCount: 0\n").unwrap();
        assert_eq!(m.original_color, Some(0xff804020));
        assert_eq!(m.source_blend, Some(5));
        assert_eq!(m.destination_blend, Some(6));
        assert_eq!(m.default_speed, Some(1.25));
        let a = &m.actions[0];
        assert_eq!(a.name, "visible");
        assert_eq!(a.loops, Some(-1));
        assert_eq!(a.speed, Some(0.5));
        assert_eq!(a.bases[0].name, "local-a");
        assert_eq!(a.bases[0].loop_min, Some(3));
        assert_eq!(a.bases[0].loop_max, Some(10));
        assert_eq!(a.bases[1].start_ms, Some(3350));
    }
    #[test]
    fn legacy_loops_and_event_scope_are_distinct() {
        let m=parse("ComActCount: 1\nEventType: 100\nStartTime: 0\nTimeSpan: -1\nFxFilePath: scene\\attached.gfx\nHookName: hook\nCombineActName: visible\nLoopCount: 1\nBaseActCount: 1\nBaseActName: underlying\nActStartTime: 0\nLoopCount: -1\nEventCount: 1\nEventType: 999\nLoopCount: 12\nFxFilePath: other.gfx\n").unwrap();
        assert!(m.original_color.is_none());
        assert!(m.default_speed.is_none());
        assert_eq!(m.actions[0].loops, Some(1));
        assert_eq!(m.actions[0].bases[0].loop_max, Some(-1));
        assert_eq!(m.events.len(), 2);
        assert!(m.events[0].action.is_none());
        assert_eq!(
            m.events[0].gfx_path().as_deref(),
            Some("scene/attached.gfx")
        );
        assert_eq!(m.events[1].action.as_deref(), Some("visible"));
        assert!(m.events[1].gfx_path().is_none());
    }
    #[test]
    fn source_backed_legacy_render_defaults_do_not_replace_explicit_fields() {
        let m = parse("").unwrap();
        assert!(m.original_color.is_none());
        assert_eq!(
            m.render_settings(),
            RenderSettings {
                original_color: u32::MAX,
                source_blend: 5,
                destination_blend: 6,
                default_speed: 1.
            }
        );
        let m = parse("OrgColor: 80402010\nSrcBlend: 2\nDestBlend: 1\nDefSpeed: 0").unwrap();
        assert_eq!(
            m.render_settings(),
            RenderSettings {
                original_color: 0x80402010,
                source_blend: 2,
                destination_blend: 1,
                default_speed: 0.
            }
        );
    }
    #[test]
    fn retail_single_base_infinite_override_and_default_bounds() {
        let m=parse("ComActCount: 2\nCombineActName: a\nLoopCount: -1\nBaseActCount: 1\nBaseActName: b\nLoopMinNum: 3\nLoopMaxNum: 8\nEventCount: 0\nCombineActName: c\nLoopCount: 1\nBaseActCount: 1\nBaseActName: d\nEventCount: 0").unwrap();
        assert_eq!(m.actions[0].base_loop_bounds(0), Some((-1, -1)));
        assert_eq!(m.actions[1].base_loop_bounds(0), Some((1, 1)));
        assert_eq!(m.actions[0].base_loop_bounds(1), None);
        // Metadata decoding is not a playback policy: preserve zero/reverse
        // speeds instead of inventing a positive-only client constraint.
        let m =
            parse("DefSpeed: 0\nComActCount: 1\nCombineActName: a\nPlaySpeed: -1\nBaseActCount: 0")
                .unwrap();
        assert_eq!(m.default_speed, Some(0.));
        assert_eq!(m.actions[0].speed, Some(-1.));
    }
    #[test]
    fn rejects_bad_counts_timing_and_nonfinite_speed() {
        for body in [
            "ComActCount: 1",
            "DefSpeed: NaN",
            "CombineActName: a\nBaseActCount: 1",
            "CombineActName: a\nBaseActCount: 1\nBaseActName: b\nLoopMinNum: 9\nLoopMaxNum: 2",
            "CombineActName: a\nBaseActCount: 1\nBaseActName: b\nActStartTime: bad",
        ] {
            assert!(parse(body).is_err(), "accepted {body}");
        }
    }
}

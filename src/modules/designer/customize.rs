//! Native `userdata/character` INI data, including the less obvious signed
//! ARGB colours and the two independent asymmetrical slider sets.

use super::catalog::{Catalog, Class, Gender};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FaceCustomize {
    pub scale_up: u8,
    pub scale_middle: u8,
    pub scale_down: u8,
    pub id_face_shape1: u16,
    pub id_face_shape2: u16,
    pub blend_face_shape: u8,
    pub scale_face_h: u8,
    pub scale_face_v: u8,
    pub id_face_tex: u16,
    pub id_faling_skin: u16,
    pub id_faling_tex: u16,
    pub offset_forehead_h: u8,
    pub offset_forehead_v: u8,
    pub offset_forehead_z: u8,
    pub rotate_forehead: u8,
    pub scale_forehead: u8,
    pub offset_yoke_bone_h: u8,
    pub offset_yoke_bone_v: u8,
    pub offset_yoke_bone_z: u8,
    pub rotate_yoke_bone: u8,
    pub scale_yoke_bone: u8,
    pub offset_cheek_h: u8,
    pub offset_cheek_v: u8,
    pub offset_cheek_z: u8,
    pub scale_cheek: u8,
    pub offset_chain_v: u8,
    pub offset_chain_z: u8,
    pub rotate_chain: u8,
    pub scale_chain_h: u8,
    pub offset_jaw_h: u8,
    pub offset_jaw_v: u8,
    pub offset_jaw_z: u8,
    pub scale_jaw_special: u8,
    pub scale_jaw_h: u8,
    pub scale_jaw_v: u8,
    pub id_third_eye: u16,
    pub id_eye_base_tex: u16,
    pub id_eye_high_tex: u16,
    pub id_eye_ball_tex: u16,
    pub id_eye_shape: u16,
    pub scale_eye_h: u8,
    pub scale_eye_v: u8,
    pub rotate_eye: u8,
    pub offset_eye_h: u8,
    pub offset_eye_v: u8,
    pub offset_eye_z: u8,
    pub scale_eye_ball: u8,
    pub scale_eye_h2: u8,
    pub scale_eye_v2: u8,
    pub rotate_eye2: u8,
    pub offset_eye_h2: u8,
    pub offset_eye_v2: u8,
    pub offset_eye_z2: u8,
    pub scale_eye_ball2: u8,
    pub id_brow_tex: u16,
    pub id_brow_shape: u16,
    pub scale_brow_h: u8,
    pub scale_brow_v: u8,
    pub rotate_brow: u8,
    pub offset_brow_h: u8,
    pub offset_brow_v: u8,
    pub offset_brow_z: u8,
    pub scale_brow_h2: u8,
    pub scale_brow_v2: u8,
    pub rotate_brow2: u8,
    pub offset_brow_h2: u8,
    pub offset_brow_v2: u8,
    pub offset_brow_z2: u8,
    pub id_nose_tex: u16,
    pub id_nose_tip_shape: u16,
    pub scale_nose_tip_h: u8,
    pub scale_nose_tip_v: u8,
    pub scale_nose_tip_z: u8,
    pub offset_nose_tip_v: u8,
    pub id_nose_bridge_shape: u16,
    pub scale_bridge_tip_h: u8,
    pub offset_bridge_tip_z: u8,
    pub id_mouth_up_lip_line: u16,
    pub id_mouth_mid_lip_line: u16,
    pub id_mouth_down_lip_line: u16,
    pub thick_up_lip: u8,
    pub thick_down_lip: u8,
    pub scale_mouth_h: u8,
    pub offset_mouth_v: u8,
    pub offset_mouth_z: u8,
    pub id_mouth_tex: u16,
    pub offset_corner_of_mouth_special: u8,
    pub scale_mouth_h2: u8,
    pub offset_corner_of_mouth_special2: u8,
    pub id_ear_shape: u16,
    pub scale_ear: u8,
    pub offset_ear_v: u8,
    pub id_hair_model: u16,
    pub id_hair_tex: u16,
    pub id_moustache_tex: u16,
    pub id_moustache_skin: u16,
    pub id_goatee_tex: u16,
    pub color_hair: u32,
    pub color_face: u32,
    pub color_eye: u32,
    pub color_brow: u32,
    pub color_mouth: u32,
    pub color_eye_ball: u32,
    pub color_moustache: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerCustomize {
    pub face: FaceCustomize,
    pub body_id: u16,
    pub color_body: u32,
    pub head_scale: u8,
    pub up_scale: u8,
    pub waist_scale: u8,
    pub arm_width: u8,
    pub leg_width: u8,
    pub breast_scale: u8,
}
impl Default for PlayerCustomize {
    fn default() -> Self {
        Self {
            face: FaceCustomize::default(),
            body_id: 0,
            color_body: u32::MAX,
            head_scale: 128,
            up_scale: 128,
            waist_scale: 128,
            arm_width: 128,
            leg_width: 128,
            breast_scale: 128,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Equipment {
    pub weapon: u32,
    pub head: u32,
    pub upper: u32,
    pub wrist: u32,
    pub lower: u32,
    pub foot: u32,
    pub fashion_head: u32,
    pub fashion_upper: u32,
    pub fashion_wrist: u32,
    pub fashion_lower: u32,
    pub fashion_foot: u32,
    pub fashion_mode: bool,
    /// Non-INI design state: zero armor slots which still request CECPlayer's
    /// class/gender default SKI. An explicit `set_armor` clears that slot's bit,
    /// making zero a genuinely empty slot without disturbing the other slots.
    pub native_default_armor_mask: u8,
    /// Section-23/24 flight item, hidden until flight is activated.
    pub flight: u32,
    /// Section-95 Ride pet, hidden until riding is activated.
    pub mount: u32,
}
impl Equipment {
    pub fn armor(&self, slot: super::catalog::ArmorSlot) -> u32 {
        use super::catalog::ArmorSlot::*;
        match slot {
            Head => self.head,
            Upper => self.upper,
            Wrist => self.wrist,
            Lower => self.lower,
            Foot => self.foot,
        }
    }
    pub fn set_armor(&mut self, slot: super::catalog::ArmorSlot, id: u32) {
        use super::catalog::ArmorSlot::*;
        match slot {
            Head => self.head = id,
            Upper => self.upper = id,
            Wrist => self.wrist = id,
            Lower => self.lower = id,
            Foot => self.foot = id,
        }
        self.set_native_default_armor(slot, false);
    }
    pub fn native_default_armor(&self, slot: super::catalog::ArmorSlot) -> bool {
        slot.skin_index()
            .is_some_and(|index| self.native_default_armor_mask & (1 << index) != 0)
    }
    pub fn set_native_default_armor(&mut self, slot: super::catalog::ArmorSlot, enabled: bool) {
        if let Some(index) = slot.skin_index() {
            let bit = 1 << index;
            if enabled {
                self.native_default_armor_mask |= bit;
            } else {
                self.native_default_armor_mask &= !bit;
            }
        }
    }
    pub fn fashion(&self, slot: super::catalog::ArmorSlot) -> u32 {
        use super::catalog::ArmorSlot::*;
        match slot {
            Head => self.fashion_head,
            Upper => self.fashion_upper,
            Wrist => self.fashion_wrist,
            Lower => self.fashion_lower,
            Foot => self.fashion_foot,
        }
    }
    pub fn set_fashion(&mut self, slot: super::catalog::ArmorSlot, id: u32) {
        use super::catalog::ArmorSlot::*;
        match slot {
            Head => self.fashion_head = id,
            Upper => self.fashion_upper = id,
            Wrist => self.fashion_wrist = id,
            Lower => self.fashion_lower = id,
            Foot => self.fashion_foot = id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CharacterDesign {
    pub class: Class,
    pub gender: Gender,
    pub customize: PlayerCustomize,
    pub equipment: Equipment,
}
impl CharacterDesign {
    pub fn new(class: Class, gender: Gender) -> Self {
        Self {
            class,
            gender,
            customize: PlayerCustomize::default(),
            equipment: Equipment::default(),
        }
    }
    pub fn from_default_ini(class: Class, gender: Gender, bytes: &[u8]) -> Result<Self, String> {
        let ini = Ini::parse(bytes)?;
        let mut result = Self::new(class, gender);
        result.customize.read_ini(&ini);
        Ok(result)
    }
    pub fn apply_preset_ini(&mut self, bytes: &[u8]) -> Result<(), String> {
        let ini = Ini::parse(bytes)?;
        // CDlgCustomizePre::LoadCustomizeData starts with a zeroed structure,
        // unlike LoadDefaultCustomizeData's neutral body scales/white skin.
        let mut preset = PlayerCustomize {
            color_body: 0,
            head_scale: 0,
            up_scale: 0,
            waist_scale: 0,
            arm_width: 0,
            leg_width: 0,
            breast_scale: 0,
            ..PlayerCustomize::default()
        };
        preset.read_ini(&ini);
        self.customize = preset;
        Ok(())
    }
    pub fn to_ini_gbk(&self) -> Result<Vec<u8>, String> {
        let text = self.customize.to_ini();
        let (bytes, _, had_errors) = encoding_rs::GBK.encode(&text);
        if had_errors {
            return Err("preset contains text that cannot be encoded as GBK".into());
        }
        Ok(bytes.into_owned())
    }
    /// Native CUS_COLOR_FACE updates both shader color fields. INI loading is
    /// intentionally separate and still preserves independently stored values.
    pub fn set_face_color(&mut self, color: u32) {
        self.customize.face.color_face = color;
        self.customize.color_body = color;
    }

    pub fn clamp(&mut self, bounds: &SliderBounds) {
        self.customize.clamp(bounds);
    }
    /// Missing IDs are retained in the design and reported to the user. The
    /// native client simply leaves the corresponding resource unchanged; this
    /// designer does the same rather than inventing a replacement asset.
    pub fn missing_face_ids(&self, catalog: &Catalog) -> Vec<String> {
        let f = &self.customize.face;
        let mut missing = Vec::new();
        let mut check = |kind: &str, id: u16, exists: bool| {
            if id != 0 && !exists {
                missing.push(format!(
                    "{kind} ID {id} is not present in PW 1.4.2 elements.data"
                ));
            }
        };
        check(
            "Face shape 1",
            f.id_face_shape1,
            catalog.face_shape(f.id_face_shape1 as u32).is_some(),
        );
        check(
            "Face shape 2",
            f.id_face_shape2,
            catalog.face_shape(f.id_face_shape2 as u32).is_some(),
        );
        check(
            "Face texture",
            f.id_face_tex,
            catalog.face_texture(f.id_face_tex as u32).is_some(),
        );
        check(
            "Eye shape",
            f.id_eye_shape,
            catalog.face_shape(f.id_eye_shape as u32).is_some(),
        );
        check(
            "Eye base texture",
            f.id_eye_base_tex,
            catalog.face_texture(f.id_eye_base_tex as u32).is_some(),
        );
        check(
            "Eye high texture",
            f.id_eye_high_tex,
            catalog.face_texture(f.id_eye_high_tex as u32).is_some(),
        );
        check(
            "Eyeball texture",
            f.id_eye_ball_tex,
            catalog.face_texture(f.id_eye_ball_tex as u32).is_some(),
        );
        check(
            "Brow shape",
            f.id_brow_shape,
            catalog.face_shape(f.id_brow_shape as u32).is_some(),
        );
        check(
            "Brow texture",
            f.id_brow_tex,
            catalog.face_texture(f.id_brow_tex as u32).is_some(),
        );
        check(
            "Nose tip shape",
            f.id_nose_tip_shape,
            catalog.face_shape(f.id_nose_tip_shape as u32).is_some(),
        );
        check(
            "Nose bridge shape",
            f.id_nose_bridge_shape,
            catalog.face_shape(f.id_nose_bridge_shape as u32).is_some(),
        );
        check(
            "Nose texture",
            f.id_nose_tex,
            catalog.face_texture(f.id_nose_tex as u32).is_some(),
        );
        check(
            "Upper lip shape",
            f.id_mouth_up_lip_line,
            catalog.face_shape(f.id_mouth_up_lip_line as u32).is_some(),
        );
        check(
            "Middle lip shape",
            f.id_mouth_mid_lip_line,
            catalog.face_shape(f.id_mouth_mid_lip_line as u32).is_some(),
        );
        check(
            "Lower lip shape",
            f.id_mouth_down_lip_line,
            catalog
                .face_shape(f.id_mouth_down_lip_line as u32)
                .is_some(),
        );
        check(
            "Mouth texture",
            f.id_mouth_tex,
            catalog.face_texture(f.id_mouth_tex as u32).is_some(),
        );
        check(
            "Ear shape",
            f.id_ear_shape,
            catalog.face_shape(f.id_ear_shape as u32).is_some(),
        );
        check(
            "Hair model",
            f.id_hair_model,
            catalog.hair_item(f.id_hair_model as u32).is_some(),
        );
        check(
            "Hair texture",
            f.id_hair_tex,
            catalog.face_texture(f.id_hair_tex as u32).is_some(),
        );
        check(
            "Moustache skin",
            f.id_moustache_skin,
            catalog.moustache_item(f.id_moustache_skin as u32).is_some(),
        );
        check(
            "Moustache texture",
            f.id_moustache_tex,
            catalog.face_texture(f.id_moustache_tex as u32).is_some(),
        );
        check(
            "Goatee texture",
            f.id_goatee_tex,
            catalog.face_texture(f.id_goatee_tex as u32).is_some(),
        );
        check(
            "Faling skin",
            f.id_faling_skin,
            catalog.faling_item(f.id_faling_skin as u32).is_some(),
        );
        check(
            "Faling texture",
            f.id_faling_tex,
            catalog.face_texture(f.id_faling_tex as u32).is_some(),
        );
        missing
    }
}

impl PlayerCustomize {
    fn read_ini(&mut self, ini: &Ini) {
        macro_rules! read {
            ($section:literal, $key:literal, $field:ident) => {
                self.$field = ini.number($section, $key, self.$field as i64) as _;
            };
        }
        macro_rules! face {
            ($section:literal, $key:literal, $field:ident) => {
                self.face.$field = ini.number($section, $key, self.face.$field as i64) as _;
            };
        }
        face!("3Parts", "scaleUp", scale_up);
        face!("3Parts", "scaleMiddle", scale_middle);
        face!("3Parts", "scaleDown", scale_down);
        face!("BlendFace", "idFaceShape1", id_face_shape1);
        face!("BlendFace", "idFaceShape2", id_face_shape2);
        face!("BlendFace", "blendFaceShape", blend_face_shape);
        face!("Face", "scaleFaceH", scale_face_h);
        face!("Face", "scaleFaceV", scale_face_v);
        face!("Face", "idFaceTex", id_face_tex);
        face!("Faling", "idFalingSkin", id_faling_skin);
        face!("Faling", "idFalingTex", id_faling_tex);
        face!("Forehead", "offsetForeheadH", offset_forehead_h);
        face!("Forehead", "offsetForeheadV", offset_forehead_v);
        face!("Forehead", "offsetForeheadZ", offset_forehead_z);
        face!("Forehead", "rotateForehead", rotate_forehead);
        face!("Forehead", "scaleForehead", scale_forehead);
        face!("YokeBone", "offsetYokeBoneH", offset_yoke_bone_h);
        face!("YokeBone", "offsetYokeBoneV", offset_yoke_bone_v);
        face!("YokeBone", "offsetYokeBoneZ", offset_yoke_bone_z);
        face!("YokeBone", "rotateYokeBone", rotate_yoke_bone);
        face!("YokeBone", "scaleYokeBone", scale_yoke_bone);
        face!("Cheek", "offsetCheekH", offset_cheek_h);
        face!("Cheek", "offsetCheekV", offset_cheek_v);
        face!("Cheek", "offsetCheekZ", offset_cheek_z);
        face!("Cheek", "scaleCheek", scale_cheek);
        face!("Chain", "offsetChainV", offset_chain_v);
        face!("Chain", "offsetChainZ", offset_chain_z);
        face!("Chain", "rotateChain", rotate_chain);
        face!("Chain", "scaleChainH", scale_chain_h);
        face!("Jaw", "offsetJawH", offset_jaw_h);
        face!("Jaw", "offsetJawV", offset_jaw_v);
        face!("Jaw", "offsetJawZ", offset_jaw_z);
        face!("Jaw", "scaleJawSpecial", scale_jaw_special);
        face!("Jaw", "scaleJawH", scale_jaw_h);
        face!("Jaw", "scaleJawV", scale_jaw_v);
        face!("Eye", "idThirdEye", id_third_eye);
        face!("Eye", "idEyeBaseTex", id_eye_base_tex);
        face!("Eye", "idEyeHighTex", id_eye_high_tex);
        face!("Eye", "idEyeBallTex", id_eye_ball_tex);
        face!("Eye", "idEyeShape", id_eye_shape);
        face!("Eye", "scaleEyeH", scale_eye_h);
        face!("Eye", "scaleEyeV", scale_eye_v);
        face!("Eye", "rotateEye", rotate_eye);
        face!("Eye", "offsetEyeH", offset_eye_h);
        face!("Eye", "offsetEyeV", offset_eye_v);
        face!("Eye", "offseteyeZ", offset_eye_z);
        face!("Eye", "scaleEyeBall", scale_eye_ball);
        face!("Eye", "scaleEyeH2", scale_eye_h2);
        face!("Eye", "scaleEyeV2", scale_eye_v2);
        face!("Eye", "rotateEye2", rotate_eye2);
        face!("Eye", "offsetEyeH2", offset_eye_h2);
        face!("Eye", "offsetEyeV2", offset_eye_v2);
        face!("Eye", "offseteyeZ2", offset_eye_z2);
        face!("Eye", "scaleEyeBall2", scale_eye_ball2);
        face!("Brow", "idBrowTex", id_brow_tex);
        face!("Brow", "idBrowShape", id_brow_shape);
        face!("Brow", "scaleBrowH", scale_brow_h);
        face!("Brow", "scaleBrowV", scale_brow_v);
        face!("Brow", "rotateBrow", rotate_brow);
        face!("Brow", "offsetBrowH", offset_brow_h);
        face!("Brow", "offsetBrowV", offset_brow_v);
        face!("Brow", "offsetBrowZ", offset_brow_z);
        face!("Brow", "scaleBrowH2", scale_brow_h2);
        face!("Brow", "scaleBrowV2", scale_brow_v2);
        face!("Brow", "rotateBrow2", rotate_brow2);
        face!("Brow", "offsetBrowH2", offset_brow_h2);
        face!("Brow", "offsetBrowV2", offset_brow_v2);
        face!("Brow", "offsetBrowZ2", offset_brow_z2);
        face!("Nose", "idNoseTex", id_nose_tex);
        face!("Nose", "idNoseTipShape", id_nose_tip_shape);
        face!("Nose", "scaleNoseTipH", scale_nose_tip_h);
        face!("Nose", "scaleNoseTipV", scale_nose_tip_v);
        face!("Nose", "scaleNoseTipZ", scale_nose_tip_z);
        face!("Nose", "offsetNoseTipV", offset_nose_tip_v);
        face!("Nose", "idNoseBridgeShape", id_nose_bridge_shape);
        face!("Nose", "scaleBridgeTipH", scale_bridge_tip_h);
        face!("Nose", "OffsetBridgeTipZ", offset_bridge_tip_z);
        face!("Mouth", "idMouthUpLipLine", id_mouth_up_lip_line);
        face!("Mouth", "idMouthMidLipLine", id_mouth_mid_lip_line);
        face!("Mouth", "idMouthDownLipLine", id_mouth_down_lip_line);
        face!("Mouth", "thickUpLip", thick_up_lip);
        face!("Mouth", "thickDownLip", thick_down_lip);
        face!("Mouth", "scaleMouthH", scale_mouth_h);
        face!("Mouth", "offsetMouthV", offset_mouth_v);
        face!("Mouth", "offsetMOuthZ", offset_mouth_z);
        face!("Mouth", "idMouthTex", id_mouth_tex);
        face!(
            "Mouth",
            "offsetCornerOfMouthSpecial",
            offset_corner_of_mouth_special
        );
        face!("Mouth", "scaleMouthH2", scale_mouth_h2);
        face!(
            "Mouth",
            "offsetCornerOfMouthSpecial2",
            offset_corner_of_mouth_special2
        );
        face!("Ear", "idEarShape", id_ear_shape);
        face!("Ear", "scaleEar", scale_ear);
        face!("Ear", "offsetEarV", offset_ear_v);
        face!("Hair", "idHairModel", id_hair_model);
        face!("Hair", "idHairTex", id_hair_tex);
        face!("Moustache", "idMoustacheTex", id_moustache_tex);
        face!("Moustache", "idMoustacheSkin", id_moustache_skin);
        face!("Moustache", "idGoateeTex", id_goatee_tex);
        face!("Color", "colorHair", color_hair);
        face!("Color", "colorFace", color_face);
        face!("Color", "colorEye", color_eye);
        face!("Color", "colorBrow", color_brow);
        face!("Color", "colorMouth", color_mouth);
        face!("Color", "colorEyeBall", color_eye_ball);
        face!("Color", "colorMoustache", color_moustache);
        read!("Body", "bodyID", body_id);
        read!("Body", "colorBody", color_body);
        read!("Body", "headScale", head_scale);
        read!("Body", "upScale", up_scale);
        read!("Body", "waistScale", waist_scale);
        read!("Body", "armWidth", arm_width);
        read!("Body", "legWidth", leg_width);
        read!("Body", "breastScale", breast_scale);
    }

    pub fn to_ini(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(2048);
        macro_rules! section {
            ($name:literal) => {
                out.push_str("[");
                out.push_str($name);
                out.push_str("]\r\n");
            };
        }
        macro_rules! write {
            ($key:literal, $value:expr) => {
                let _ = std::write!(out, "{} = {}\r\n", $key, $value);
            };
        }
        macro_rules! color {
            ($key:literal, $value:expr) => {
                write!($key, $value as i32)
            };
        }
        section!("3Parts");
        write!("scaleUp", self.face.scale_up);
        write!("scaleMiddle", self.face.scale_middle);
        write!("scaleDown", self.face.scale_down);
        section!("BlendFace");
        write!("idFaceShape1", self.face.id_face_shape1);
        write!("idFaceShape2", self.face.id_face_shape2);
        write!("blendFaceShape", self.face.blend_face_shape);
        section!("Face");
        write!("scaleFaceH", self.face.scale_face_h);
        write!("scaleFaceV", self.face.scale_face_v);
        write!("idFaceTex", self.face.id_face_tex);
        section!("Faling");
        write!("idFalingSkin", self.face.id_faling_skin);
        write!("idFalingTex", self.face.id_faling_tex);
        section!("Forehead");
        write!("offsetForeheadH", self.face.offset_forehead_h);
        write!("offsetForeheadV", self.face.offset_forehead_v);
        write!("offsetForeheadZ", self.face.offset_forehead_z);
        write!("rotateForehead", self.face.rotate_forehead);
        write!("scaleForehead", self.face.scale_forehead);
        section!("YokeBone");
        write!("offsetYokeBoneH", self.face.offset_yoke_bone_h);
        write!("offsetYokeBoneV", self.face.offset_yoke_bone_v);
        write!("offsetYokeBoneZ", self.face.offset_yoke_bone_z);
        write!("rotateYokeBone", self.face.rotate_yoke_bone);
        write!("scaleYokeBone", self.face.scale_yoke_bone);
        section!("Cheek");
        write!("offsetCheekH", self.face.offset_cheek_h);
        write!("offsetCheekV", self.face.offset_cheek_v);
        write!("offsetCheekZ", self.face.offset_cheek_z);
        write!("scaleCheek", self.face.scale_cheek);
        section!("Chain");
        write!("offsetChainV", self.face.offset_chain_v);
        write!("offsetChainZ", self.face.offset_chain_z);
        write!("rotateChain", self.face.rotate_chain);
        write!("scaleChainH", self.face.scale_chain_h);
        section!("Jaw");
        write!("offsetJawH", self.face.offset_jaw_h);
        write!("offsetJawV", self.face.offset_jaw_v);
        write!("offsetJawZ", self.face.offset_jaw_z);
        write!("scaleJawSpecial", self.face.scale_jaw_special);
        write!("scaleJawH", self.face.scale_jaw_h);
        write!("scaleJawV", self.face.scale_jaw_v);
        section!("Eye");
        write!("idThirdEye", self.face.id_third_eye);
        write!("idEyeBaseTex", self.face.id_eye_base_tex);
        write!("idEyeHighTex", self.face.id_eye_high_tex);
        write!("idEyeBallTex", self.face.id_eye_ball_tex);
        write!("idEyeShape", self.face.id_eye_shape);
        write!("scaleEyeH", self.face.scale_eye_h);
        write!("scaleEyeV", self.face.scale_eye_v);
        write!("rotateEye", self.face.rotate_eye);
        write!("offsetEyeH", self.face.offset_eye_h);
        write!("offsetEyeV", self.face.offset_eye_v);
        write!("offseteyeZ", self.face.offset_eye_z);
        write!("scaleEyeBall", self.face.scale_eye_ball);
        write!("scaleEyeH2", self.face.scale_eye_h2);
        write!("scaleEyeV2", self.face.scale_eye_v2);
        write!("rotateEye2", self.face.rotate_eye2);
        write!("offsetEyeH2", self.face.offset_eye_h2);
        write!("offsetEyeV2", self.face.offset_eye_v2);
        write!("offseteyeZ2", self.face.offset_eye_z2);
        write!("scaleEyeBall2", self.face.scale_eye_ball2);
        section!("Brow");
        write!("idBrowTex", self.face.id_brow_tex);
        write!("idBrowShape", self.face.id_brow_shape);
        write!("scaleBrowH", self.face.scale_brow_h);
        write!("scaleBrowV", self.face.scale_brow_v);
        write!("rotateBrow", self.face.rotate_brow);
        write!("offsetBrowH", self.face.offset_brow_h);
        write!("offsetBrowV", self.face.offset_brow_v);
        write!("offsetBrowZ", self.face.offset_brow_z);
        write!("scaleBrowH2", self.face.scale_brow_h2);
        write!("scaleBrowV2", self.face.scale_brow_v2);
        write!("rotateBrow2", self.face.rotate_brow2);
        write!("offsetBrowH2", self.face.offset_brow_h2);
        write!("offsetBrowV2", self.face.offset_brow_v2);
        write!("offsetBrowZ2", self.face.offset_brow_z2);
        section!("Nose");
        write!("idNoseTex", self.face.id_nose_tex);
        write!("idNoseTipShape", self.face.id_nose_tip_shape);
        write!("scaleNoseTipH", self.face.scale_nose_tip_h);
        write!("scaleNoseTipV", self.face.scale_nose_tip_v);
        write!("scaleNoseTipZ", self.face.scale_nose_tip_z);
        write!("offsetNoseTipV", self.face.offset_nose_tip_v);
        write!("idNoseBridgeShape", self.face.id_nose_bridge_shape);
        write!("scaleBridgeTipH", self.face.scale_bridge_tip_h);
        write!("offsetBridgeTipZ", self.face.offset_bridge_tip_z);
        section!("Mouth");
        write!("idMouthUpLipLine", self.face.id_mouth_up_lip_line);
        write!("idMouthMidLipLine", self.face.id_mouth_mid_lip_line);
        write!("idMouthDownLipLine", self.face.id_mouth_down_lip_line);
        write!("thickUpLip", self.face.thick_up_lip);
        write!("thickDownLip", self.face.thick_down_lip);
        write!("scaleMouthH", self.face.scale_mouth_h);
        write!("offsetMouthV", self.face.offset_mouth_v);
        write!("offsetMOuthZ", self.face.offset_mouth_z);
        write!("idMouthTex", self.face.id_mouth_tex);
        write!(
            "offsetCornerOfMouthSpecial",
            self.face.offset_corner_of_mouth_special
        );
        write!("scaleMouthH2", self.face.scale_mouth_h2);
        write!(
            "offsetCornerOfMouthSpecial2",
            self.face.offset_corner_of_mouth_special2
        );
        section!("Ear");
        write!("idEarShape", self.face.id_ear_shape);
        write!("scaleEar", self.face.scale_ear);
        write!("offsetEarV", self.face.offset_ear_v);
        section!("Hair");
        write!("idHairModel", self.face.id_hair_model);
        write!("idHairTex", self.face.id_hair_tex);
        section!("Moustache");
        write!("idMoustacheTex", self.face.id_moustache_tex);
        write!("idMoustacheSkin", self.face.id_moustache_skin);
        write!("idGoateeTex", self.face.id_goatee_tex);
        section!("Color");
        color!("colorHair", self.face.color_hair);
        color!("colorFace", self.face.color_face);
        color!("colorEye", self.face.color_eye);
        color!("colorBrow", self.face.color_brow);
        color!("colorMouth", self.face.color_mouth);
        color!("colorEyeBall", self.face.color_eye_ball);
        color!("colorMoustache", self.face.color_moustache);
        section!("Body");
        write!("bodyID", self.body_id);
        color!("colorBody", self.color_body);
        write!("headScale", self.head_scale);
        write!("upScale", self.up_scale);
        write!("waistScale", self.waist_scale);
        write!("armWidth", self.arm_width);
        write!("legWidth", self.leg_width);
        write!("breastScale", self.breast_scale);
        out
    }

    fn clamp(&mut self, bounds: &SliderBounds) {
        macro_rules! clamp {
            ($section:literal, $key:literal, $field:ident) => {
                bounds.clamp($section, $key, &mut self.$field);
            };
        }
        macro_rules! face {
            ($section:literal, $key:literal, $field:ident) => {
                bounds.clamp($section, $key, &mut self.face.$field);
            };
        }
        face!("3Parts", "ScaleUpPart", scale_up);
        face!("3Parts", "ScaleMiddlePart", scale_middle);
        face!("3Parts", "ScaleDownPart", scale_down);
        face!("Face", "ScaleFaceH", scale_face_h);
        face!("Face", "ScaleFaceV", scale_face_v);
        face!("Forehead", "OffsetForeheadH", offset_forehead_h);
        face!("Forehead", "OffsetForeheadV", offset_forehead_v);
        face!("Forehead", "OffsetForeheadZ", offset_forehead_z);
        face!("Forehead", "RotateForehead", rotate_forehead);
        face!("Forehead", "ScaleForehead", scale_forehead);
        face!("YokeBone", "OffsetYokeBoneH", offset_yoke_bone_h);
        face!("YokeBone", "OffsetYokeBoneV", offset_yoke_bone_v);
        face!("YokeBone", "OffsetYokeBoneZ", offset_yoke_bone_z);
        face!("YokeBone", "RotateYokeBone", rotate_yoke_bone);
        face!("YokeBone", "ScaleYokeBone", scale_yoke_bone);
        face!("Cheek", "OffsetCheekH", offset_cheek_h);
        face!("Cheek", "OffsetCheekV", offset_cheek_v);
        face!("Cheek", "OffsetCheekZ", offset_cheek_z);
        face!("Cheek", "ScaleCheek", scale_cheek);
        face!("Chain", "OffsetChainV", offset_chain_v);
        face!("Chain", "OffsetChainZ", offset_chain_z);
        face!("Chain", "RotateChain", rotate_chain);
        face!("Chain", "ScaleChainH", scale_chain_h);
        face!("Jaw", "OffsetJawH", offset_jaw_h);
        face!("Jaw", "OffsetJawV", offset_jaw_v);
        face!("Jaw", "OffsetJawZ", offset_jaw_z);
        face!("Jaw", "ScaleJawSpecial", scale_jaw_special);
        face!("Jaw", "ScaleJawH", scale_jaw_h);
        face!("Jaw", "ScaleJawV", scale_jaw_v);
        face!("Eye", "ScaleEyeH", scale_eye_h);
        face!("Eye", "ScaleEyeV", scale_eye_v);
        face!("Eye", "RotateEye", rotate_eye);
        face!("Eye", "OffsetEyeH", offset_eye_h);
        face!("Eye", "OffsetEyeV", offset_eye_v);
        face!("Eye", "OffsetEyeZ", offset_eye_z);
        face!("Eye", "ScaleEyeBall", scale_eye_ball);
        face!("Eye", "ScaleEyeH", scale_eye_h2);
        face!("Eye", "ScaleEyeV", scale_eye_v2);
        face!("Eye", "RotateEye", rotate_eye2);
        face!("Eye", "OffsetEyeH", offset_eye_h2);
        face!("Eye", "OffsetEyeV", offset_eye_v2);
        face!("Eye", "OffsetEyeZ", offset_eye_z2);
        face!("Eye", "ScaleEyeBall", scale_eye_ball2);
        face!("Brow", "ScaleBrowH", scale_brow_h);
        face!("Brow", "ScaleBrowV", scale_brow_v);
        face!("Brow", "RotateBrow", rotate_brow);
        face!("Brow", "OffsetBrowH", offset_brow_h);
        face!("Brow", "OffsetBrowV", offset_brow_v);
        face!("Brow", "OffsetBrowZ", offset_brow_z);
        face!("Brow", "ScaleBrowH", scale_brow_h2);
        face!("Brow", "ScaleBrowV", scale_brow_v2);
        face!("Brow", "RotateBrow", rotate_brow2);
        face!("Brow", "OffsetBrowH", offset_brow_h2);
        face!("Brow", "OffsetBrowV", offset_brow_v2);
        face!("Brow", "OffsetBrowZ", offset_brow_z2);
        face!("Nose", "ScaleNoseTipH", scale_nose_tip_h);
        face!("Nose", "ScaleNoseTipV", scale_nose_tip_v);
        face!("Nose", "ScaleNoseTipZ", scale_nose_tip_z);
        face!("Nose", "OffsetNoseTipV", offset_nose_tip_v);
        face!("Nose", "ScaleBridgeTipH", scale_bridge_tip_h);
        face!("Nose", "OffsetBridgeTipZ", offset_bridge_tip_z);
        face!("Mouth", "ThickUpLip", thick_up_lip);
        face!("Mouth", "ThickDownLip", thick_down_lip);
        face!("Mouth", "ScaleMouthH", scale_mouth_h);
        face!("Mouth", "OffsetMouthV", offset_mouth_v);
        face!("Mouth", "OffsetMouthZ", offset_mouth_z);
        face!(
            "Mouth",
            "OffsetCornerOfMouthSpecial",
            offset_corner_of_mouth_special
        );
        face!("Mouth", "ScaleMouthH2", scale_mouth_h2);
        face!(
            "Mouth",
            "OffsetCornerOfMouthSpecial2",
            offset_corner_of_mouth_special2
        );
        face!("Ear", "ScaleEar", scale_ear);
        face!("Ear", "OffsetEarV", offset_ear_v);
        clamp!("Body", "HeadScale", head_scale);
        clamp!("Body", "UpScale", up_scale);
        clamp!("Body", "WaistScale", waist_scale);
        clamp!("Body", "ArmWidth", arm_width);
        clamp!("Body", "LegWidth", leg_width);
        clamp!("Body", "BreastScale", breast_scale);
    }
}

#[derive(Clone, Debug)]
pub struct Ini {
    values: std::collections::HashMap<(String, String), String>,
}
impl Ini {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let (text, _, had_errors) = encoding_rs::GBK.decode(bytes);
        if had_errors {
            return Err("character INI is not valid GBK text".into());
        }
        let mut values = std::collections::HashMap::with_capacity(bytes.len() / 24);
        let mut section = String::new();
        for original in text.lines() {
            let line = original.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                let s = line[1..line.len() - 1].trim();
                section = if s.is_ascii() {
                    s.to_ascii_lowercase()
                } else {
                    s.to_lowercase()
                };
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let k = key.trim();
                let key_lower = if k.is_ascii() {
                    k.to_ascii_lowercase()
                } else {
                    k.to_lowercase()
                };
                values.insert((section.clone(), key_lower), value.trim().to_owned());
            }
        }
        Ok(Self { values })
    }
    pub fn number(&self, section: &str, key: &str, default: i64) -> i64 {
        let s = if section.is_ascii() {
            section.to_ascii_lowercase()
        } else {
            section.to_lowercase()
        };
        let k = if key.is_ascii() {
            key.to_ascii_lowercase()
        } else {
            key.to_lowercase()
        };
        self.values
            .get(&(s, k))
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }
    pub fn range(&self, section: &str, key: &str) -> (u8, u8) {
        let mut key_buf = String::with_capacity(key.len() + 3);
        key_buf.push_str(key);
        key_buf.push_str("Min");
        let min = self.number(section, &key_buf, 0).clamp(0, 255) as u8;
        key_buf.truncate(key.len());
        key_buf.push_str("Max");
        let max = self.number(section, &key_buf, 255).clamp(0, 255) as u8;
        if min <= max {
            (min, max)
        } else {
            (max, min)
        }
    }
}

#[derive(Clone, Debug)]
pub struct SliderBounds {
    ini: Ini,
}
impl SliderBounds {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        Ok(Self {
            ini: Ini::parse(bytes)?,
        })
    }
    pub fn range(&self, section: &str, key: &str) -> std::ops::RangeInclusive<i32> {
        let (min, max) = self.ini.range(section, key);
        min as i32..=max as i32
    }
    pub fn clamp(&self, section: &str, key: &str, value: &mut u8) {
        let (min, max) = self.ini.range(section, key);
        *value = (*value).clamp(min, max);
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalog::ArmorSlot;
    use super::*;

    #[test]
    fn authentic_default_round_trip_preserves_ids_sliders_and_signed_argb() {
        let path =
            std::path::Path::new("google_drive/pw142/element/userdata/character/character00.ini");
        if !path.exists() {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let design =
            CharacterDesign::from_default_ini(Class::Blademaster, Gender::Male, &bytes).unwrap();
        assert_eq!(design.customize.face.id_face_shape1, 62);
        assert_eq!(design.customize.face.id_hair_model, 265);
        assert_eq!(design.customize.face.color_hair, -11517643i32 as u32);
        assert_eq!(design.customize.color_body, -593687i32 as u32);
        let saved = design.to_ini_gbk().unwrap();
        let reread =
            CharacterDesign::from_default_ini(Class::Blademaster, Gender::Male, &saved).unwrap();
        assert_eq!(design.customize, reread.customize);
    }
    #[test]
    fn preset_missing_body_fields_retain_native_zero_not_neutral_substitution() {
        let mut design = CharacterDesign::new(Class::Blademaster, Gender::Male);
        design.equipment.weapon = 6;
        design.equipment.upper = 100;
        design.equipment.flight = 2092;
        design.equipment.mount = 9061;
        let equipment = design.equipment.clone();
        design
            .apply_preset_ini(b"[Body]\r\nbodyID = 2\r\n")
            .unwrap();
        assert_eq!(design.customize.body_id, 2);
        assert_eq!(design.customize.head_scale, 0);
        assert_eq!(design.customize.color_body, 0);
        assert_eq!(design.equipment, equipment);
    }
    #[test]
    fn native_face_color_command_synchronizes_body_but_ini_keeps_stored_values() {
        let mut design = CharacterDesign::new(Class::Blademaster, Gender::Male);
        design.set_face_color(0xfff6f0e9);
        assert_eq!(design.customize.face.color_face, 0xfff6f0e9);
        assert_eq!(design.customize.color_body, 0xfff6f0e9);
        design
            .apply_preset_ini(b"[Color]\r\ncolorFace = 100\r\n[Body]\r\ncolorBody = 200\r\n")
            .unwrap();
        assert_eq!(design.customize.face.color_face, 100);
        assert_eq!(design.customize.color_body, 200);
    }

    #[test]
    fn malformed_preset_does_not_partially_replace_the_character() {
        let mut design = CharacterDesign::new(Class::Blademaster, Gender::Male);
        design.equipment.weapon = 6;
        design.equipment.flight = 2092;
        design.equipment.mount = 9061;
        design.customize.body_id = 7;
        let customize = design.customize.clone();
        let equipment = design.equipment.clone();
        assert!(design
            .apply_preset_ini(b"[Body]\r\nbodyID = 2\r\n\x81")
            .is_err());
        assert_eq!(design.customize, customize);
        assert_eq!(design.equipment, equipment);
    }
    #[test]
    fn ini_keys_match_native_mixed_case_spellings() {
        let mut design = CharacterDesign::new(Class::Blademaster, Gender::Male);
        design.apply_preset_ini(b"[Eye]\r\noffseteyeZ = 177\r\n[Nose]\r\nOffsetBridgeTipZ = 166\r\n[Mouth]\r\noffsetMOuthZ = 155\r\n").unwrap();
        assert_eq!(design.customize.face.offset_eye_z, 177);
        assert_eq!(design.customize.face.offset_bridge_tip_z, 166);
        assert_eq!(design.customize.face.offset_mouth_z, 155);
    }

    #[test]
    fn explicit_armor_assignment_clears_only_that_native_default_flag() {
        let mut equipment = Equipment::default();
        equipment.set_native_default_armor(ArmorSlot::Upper, true);
        equipment.set_native_default_armor(ArmorSlot::Wrist, true);
        equipment.set_armor(ArmorSlot::Upper, 0);
        assert!(!equipment.native_default_armor(ArmorSlot::Upper));
        assert!(equipment.native_default_armor(ArmorSlot::Wrist));
    }
}

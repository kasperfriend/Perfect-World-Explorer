//! Egui controls for every stored native customization and equipment field.

use super::{
    catalog::{ArmorSlot, Catalog},
    customize::{CharacterDesign, SliderBounds},
};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct ControlState {
    searches: HashMap<String, String>,
    open_picker: Option<String>,
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    design: &mut CharacterDesign,
    catalog: &Catalog,
    bounds: &SliderBounds,
    state: &mut ControlState,
) -> bool {
    let mut changed = false;
    macro_rules! slider {
        ($ui:ident, $section:literal, $key:literal, $field:ident, $label:literal) => {{
            let range = bounds.range($section, $key);
            let mut value = design.customize.face.$field as i32;
            if $ui
                .add(egui::Slider::new(&mut value, range).text($label))
                .changed()
            {
                design.customize.face.$field = value.clamp(0, 255) as u8;
                changed = true;
            }
        }};
    }
    macro_rules! body_slider {
        ($ui:ident, $key:literal, $field:ident, $label:literal) => {{
            let range = bounds.range("Body", $key);
            let mut value = design.customize.$field as i32;
            if $ui
                .add(egui::Slider::new(&mut value, range).text($label))
                .changed()
            {
                design.customize.$field = value.clamp(0, 255) as u8;
                changed = true;
            }
        }};
    }
    macro_rules! id {
        ($ui:ident, $field:ident, $label:literal) => {{
            if $ui
                .add(egui::DragValue::new(&mut design.customize.face.$field).speed(1))
                .changed()
            {
                changed = true;
            }
            $ui.label($label);
        }};
    }

    egui::CollapsingHeader::new("Body and proportions")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Body variant ID");
                if ui
                    .add(egui::DragValue::new(&mut design.customize.body_id).speed(1))
                    .changed()
                {
                    changed = true;
                }
            });
            changed |= color(ui, "Body color (ARGB)", &mut design.customize.color_body);
            body_slider!(ui, "headScale", head_scale, "Head scale");
            body_slider!(ui, "upScale", up_scale, "Upper torso");
            body_slider!(ui, "waistScale", waist_scale, "Waist");
            body_slider!(ui, "armWidth", arm_width, "Arm width");
            body_slider!(ui, "legWidth", leg_width, "Leg width");
            body_slider!(ui, "breastScale", breast_scale, "Breast scale");
            slider!(ui, "3Parts", "scaleUp", scale_up, "Face upper third");
            slider!(
                ui,
                "3Parts",
                "scaleMiddle",
                scale_middle,
                "Face middle third"
            );
            slider!(ui, "3Parts", "scaleDown", scale_down, "Face lower third");
            slider!(ui, "Face", "scaleFaceH", scale_face_h, "Face width");
            slider!(ui, "Face", "scaleFaceV", scale_face_v, "Face height");
            let mut face_color = design.customize.face.color_face;
            if color(ui, "Face color", &mut face_color) {
                design.set_face_color(face_color);
                changed = true;
            }
        });

    egui::CollapsingHeader::new("Face shape and complexion").show(ui, |ui| {
        changed |= shape_combo(ui, state, catalog, design, 0, "Face shape 1", 0);
        changed |= shape_combo(ui, state, catalog, design, 0, "Face shape 2", 1);
        slider!(
            ui,
            "BlendFace",
            "blendFaceShape",
            blend_face_shape,
            "Shape blend"
        );
        changed |= texture_combo(ui, state, catalog, design, 0, "Face texture", FaceId::Face);
        ui.horizontal(|ui| {
            id!(ui, id_third_eye, "Third-eye GFX ID");
        });
    });

    egui::CollapsingHeader::new("Forehead, temples and cheeks").show(ui, |ui| {
        slider!(
            ui,
            "Forehead",
            "offsetForeheadH",
            offset_forehead_h,
            "Forehead horizontal"
        );
        slider!(
            ui,
            "Forehead",
            "offsetForeheadV",
            offset_forehead_v,
            "Forehead vertical"
        );
        slider!(
            ui,
            "Forehead",
            "offsetForeheadZ",
            offset_forehead_z,
            "Forehead depth"
        );
        slider!(
            ui,
            "Forehead",
            "rotateForehead",
            rotate_forehead,
            "Forehead rotation"
        );
        slider!(
            ui,
            "Forehead",
            "scaleForehead",
            scale_forehead,
            "Forehead scale"
        );
        slider!(
            ui,
            "YokeBone",
            "offsetYokeBoneH",
            offset_yoke_bone_h,
            "Temple horizontal"
        );
        slider!(
            ui,
            "YokeBone",
            "offsetYokeBoneV",
            offset_yoke_bone_v,
            "Temple vertical"
        );
        slider!(
            ui,
            "YokeBone",
            "offsetYokeBoneZ",
            offset_yoke_bone_z,
            "Temple depth"
        );
        slider!(
            ui,
            "YokeBone",
            "rotateYokeBone",
            rotate_yoke_bone,
            "Temple rotation"
        );
        slider!(
            ui,
            "YokeBone",
            "scaleYokeBone",
            scale_yoke_bone,
            "Temple scale"
        );
        slider!(
            ui,
            "Cheek",
            "offsetCheekH",
            offset_cheek_h,
            "Cheek horizontal"
        );
        slider!(
            ui,
            "Cheek",
            "offsetCheekV",
            offset_cheek_v,
            "Cheek vertical"
        );
        slider!(ui, "Cheek", "offsetCheekZ", offset_cheek_z, "Cheek depth");
        slider!(ui, "Cheek", "scaleCheek", scale_cheek, "Cheek scale");
    });

    egui::CollapsingHeader::new("Chin and jaw").show(ui, |ui| {
        slider!(ui, "Chain", "offsetChainV", offset_chain_v, "Chin vertical");
        slider!(ui, "Chain", "offsetChainZ", offset_chain_z, "Chin depth");
        slider!(ui, "Chain", "rotateChain", rotate_chain, "Chin rotation");
        slider!(ui, "Chain", "scaleChainH", scale_chain_h, "Chin width");
        slider!(ui, "Jaw", "offsetJawH", offset_jaw_h, "Jaw horizontal");
        slider!(ui, "Jaw", "offsetJawV", offset_jaw_v, "Jaw vertical");
        slider!(ui, "Jaw", "offsetJawZ", offset_jaw_z, "Jaw depth");
        slider!(
            ui,
            "Jaw",
            "scaleJawSpecial",
            scale_jaw_special,
            "Jaw special scale"
        );
        slider!(ui, "Jaw", "scaleJawH", scale_jaw_h, "Jaw width");
        slider!(ui, "Jaw", "scaleJawV", scale_jaw_v, "Jaw height");
    });

    egui::CollapsingHeader::new("Eyes and eyelids").show(ui, |ui| {
        changed |= shape_combo(ui, state, catalog, design, 1, "Eye shape", 2);
        changed |= texture_combo(
            ui,
            state,
            catalog,
            design,
            1,
            "Eye base texture",
            FaceId::EyeBase,
        );
        changed |= texture_combo(
            ui,
            state,
            catalog,
            design,
            1,
            "Eye highlight texture",
            FaceId::EyeHigh,
        );
        changed |= texture_combo(
            ui,
            state,
            catalog,
            design,
            6,
            "Eyeball texture",
            FaceId::Eyeball,
        );
        changed |= color(ui, "Eyelid color", &mut design.customize.face.color_eye);
        changed |= color(
            ui,
            "Eyeball color",
            &mut design.customize.face.color_eye_ball,
        );
        ui.label("Right eye / first native controller set");
        slider!(ui, "Eye", "scaleEyeH", scale_eye_h, "Width");
        slider!(ui, "Eye", "scaleEyeV", scale_eye_v, "Height");
        slider!(ui, "Eye", "rotateEye", rotate_eye, "Rotation");
        slider!(ui, "Eye", "offsetEyeH", offset_eye_h, "Horizontal offset");
        slider!(ui, "Eye", "offsetEyeV", offset_eye_v, "Vertical offset");
        slider!(ui, "Eye", "offseteyeZ", offset_eye_z, "Depth");
        slider!(ui, "Eye", "scaleEyeBall", scale_eye_ball, "Eyeball scale");
        ui.label("Left eye / second native controller set");
        slider!(ui, "Eye", "scaleEyeH2", scale_eye_h2, "Width 2");
        slider!(ui, "Eye", "scaleEyeV2", scale_eye_v2, "Height 2");
        slider!(ui, "Eye", "rotateEye2", rotate_eye2, "Rotation 2");
        slider!(
            ui,
            "Eye",
            "offsetEyeH2",
            offset_eye_h2,
            "Horizontal offset 2"
        );
        slider!(ui, "Eye", "offsetEyeV2", offset_eye_v2, "Vertical offset 2");
        slider!(ui, "Eye", "offseteyeZ2", offset_eye_z2, "Depth 2");
        slider!(
            ui,
            "Eye",
            "scaleEyeBall2",
            scale_eye_ball2,
            "Eyeball scale 2"
        );
    });

    egui::CollapsingHeader::new("Eyebrows").show(ui, |ui| {
        changed |= shape_combo(ui, state, catalog, design, 2, "Brow shape", 3);
        changed |= texture_combo(ui, state, catalog, design, 3, "Brow texture", FaceId::Brow);
        changed |= color(ui, "Brow color", &mut design.customize.face.color_brow);
        slider!(ui, "Brow", "scaleBrowH", scale_brow_h, "Width");
        slider!(ui, "Brow", "scaleBrowV", scale_brow_v, "Height");
        slider!(ui, "Brow", "rotateBrow", rotate_brow, "Rotation");
        slider!(
            ui,
            "Brow",
            "offsetBrowH",
            offset_brow_h,
            "Horizontal offset"
        );
        slider!(ui, "Brow", "offsetBrowV", offset_brow_v, "Vertical offset");
        slider!(ui, "Brow", "offsetBrowZ", offset_brow_z, "Depth");
        slider!(ui, "Brow", "scaleBrowH2", scale_brow_h2, "Width 2");
        slider!(ui, "Brow", "scaleBrowV2", scale_brow_v2, "Height 2");
        slider!(ui, "Brow", "rotateBrow2", rotate_brow2, "Rotation 2");
        slider!(
            ui,
            "Brow",
            "offsetBrowH2",
            offset_brow_h2,
            "Horizontal offset 2"
        );
        slider!(
            ui,
            "Brow",
            "offsetBrowV2",
            offset_brow_v2,
            "Vertical offset 2"
        );
        slider!(ui, "Brow", "offsetBrowZ2", offset_brow_z2, "Depth 2");
    });

    egui::CollapsingHeader::new("Nose").show(ui, |ui| {
        changed |= shape_combo(ui, state, catalog, design, 3, "Nose tip shape", 4);
        changed |= shape_combo(ui, state, catalog, design, 4, "Nose bridge shape", 5);
        changed |= texture_combo(ui, state, catalog, design, 5, "Nose texture", FaceId::Nose);
        slider!(ui, "Nose", "scaleNoseTipH", scale_nose_tip_h, "Tip width");
        slider!(ui, "Nose", "scaleNoseTipV", scale_nose_tip_v, "Tip height");
        slider!(ui, "Nose", "scaleNoseTipZ", scale_nose_tip_z, "Tip depth");
        slider!(
            ui,
            "Nose",
            "offsetNoseTipV",
            offset_nose_tip_v,
            "Tip vertical offset"
        );
        slider!(
            ui,
            "Nose",
            "scaleBridgeTipH",
            scale_bridge_tip_h,
            "Bridge width"
        );
        slider!(
            ui,
            "Nose",
            "OffsetBridgeTipZ",
            offset_bridge_tip_z,
            "Bridge depth"
        );
    });

    egui::CollapsingHeader::new("Mouth").show(ui, |ui| {
        changed |= shape_combo(ui, state, catalog, design, 5, "Upper lip line", 6);
        changed |= shape_combo(ui, state, catalog, design, 6, "Middle lip line", 7);
        changed |= shape_combo(ui, state, catalog, design, 7, "Lower lip line", 8);
        changed |= texture_combo(
            ui,
            state,
            catalog,
            design,
            4,
            "Mouth texture",
            FaceId::Mouth,
        );
        changed |= color(ui, "Mouth color", &mut design.customize.face.color_mouth);
        slider!(
            ui,
            "Mouth",
            "thickUpLip",
            thick_up_lip,
            "Upper lip thickness"
        );
        slider!(
            ui,
            "Mouth",
            "thickDownLip",
            thick_down_lip,
            "Lower lip thickness"
        );
        slider!(ui, "Mouth", "scaleMouthH", scale_mouth_h, "Width");
        slider!(
            ui,
            "Mouth",
            "offsetMouthV",
            offset_mouth_v,
            "Vertical offset"
        );
        slider!(ui, "Mouth", "offsetMOuthZ", offset_mouth_z, "Depth");
        slider!(
            ui,
            "Mouth",
            "offsetCornerOfMouthSpecial",
            offset_corner_of_mouth_special,
            "Corner offset"
        );
        slider!(ui, "Mouth", "scaleMouthH2", scale_mouth_h2, "Width 2");
        slider!(
            ui,
            "Mouth",
            "offsetCornerOfMouthSpecial2",
            offset_corner_of_mouth_special2,
            "Corner offset 2"
        );
    });

    egui::CollapsingHeader::new("Ears, hair and facial hair").show(ui, |ui| {
        changed |= shape_combo(ui, state, catalog, design, 8, "Ear shape", 9);
        slider!(ui, "Ear", "scaleEar", scale_ear, "Ear scale");
        slider!(ui, "Ear", "offsetEarV", offset_ear_v, "Ear vertical offset");
        changed |= hair_combo(ui, state, catalog, design);
        changed |= texture_combo(ui, state, catalog, design, 8, "Hair texture", FaceId::Hair);
        changed |= color(ui, "Hair color", &mut design.customize.face.color_hair);
        ui.horizontal(|ui| {
            id!(ui, id_moustache_skin, "Goatee skin ID");
        });
        changed |= texture_combo(
            ui,
            state,
            catalog,
            design,
            9,
            "Goatee texture",
            FaceId::Goatee,
        );
        ui.horizontal(|ui| {
            id!(ui, id_faling_skin, "Faling skin ID");
        });
        changed |= texture_combo(
            ui,
            state,
            catalog,
            design,
            10,
            "Faling texture",
            FaceId::Faling,
        );
        changed |= texture_combo(
            ui,
            state,
            catalog,
            design,
            7,
            "Moustache texture",
            FaceId::Moustache,
        );
        changed |= color(
            ui,
            "Facial-hair color",
            &mut design.customize.face.color_moustache,
        );
    });

    egui::CollapsingHeader::new("Armor, fashion and weapons").show(ui, |ui| {
        if ui
            .checkbox(&mut design.equipment.fashion_mode, "Use fashion slots")
            .changed()
        {
            changed = true;
        }
        changed |= weapon_combo(ui, state, catalog, design);
        for (slot, label) in [
            (ArmorSlot::Head, "Head"),
            (ArmorSlot::Upper, "Upper body"),
            (ArmorSlot::Wrist, "Wrist"),
            (ArmorSlot::Lower, "Lower body"),
            (ArmorSlot::Foot, "Foot"),
        ] {
            changed |= armor_combo(ui, state, catalog, design, slot, label);
            changed |= fashion_combo(ui, state, catalog, design, slot, label);
        }
    });

    egui::CollapsingHeader::new("Mounts and flight")
        .default_open(true)
        .show(ui, |ui| {
            changed |= flight_combo(ui, state, catalog, design);
            changed |= mount_combo(ui, state, catalog, design);
            ui.label(
                egui::RichText::new(
                    "Equip a travel item here, then use the Walker travel controls to activate it.",
                )
                .small()
                .weak(),
            );
        });
    changed
}

#[derive(Clone, Copy)]
enum FaceId {
    Face,
    EyeBase,
    EyeHigh,
    Eyeball,
    Brow,
    Nose,
    Mouth,
    Hair,
    Goatee,
    Faling,
    Moustache,
}

fn item_matches(id: u32, name: &str, lowercase_query: &str) -> bool {
    if lowercase_query.is_empty() {
        return true;
    }
    if lowercase_query.bytes().all(|b| b.is_ascii_digit()) {
        let mut buf = [0u8; 10];
        let mut n = id;
        let mut i = buf.len();
        loop {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        if let Ok(id_str) = std::str::from_utf8(&buf[i..]) {
            if id_str.contains(lowercase_query) {
                return true;
            }
        }
    }
    if name.is_ascii() && lowercase_query.is_ascii() {
        let nb = name.as_bytes();
        let qb = lowercase_query.as_bytes();
        return nb.windows(qb.len()).any(|w| w.eq_ignore_ascii_case(qb));
    }
    name.to_lowercase().contains(lowercase_query)
}

#[allow(clippy::too_many_arguments)]
fn item_combo(
    ui: &mut egui::Ui,
    state: &mut ControlState,
    label: String,
    current: u32,
    selected: String,
    zero: Option<(u32, String)>,
    options: impl FnOnce(&str) -> Vec<(u32, String)>,
    mut set: impl FnMut(u32),
) -> bool {
    let mut changed = false;
    let key = label.clone();
    let button = ui
        .horizontal(|ui| {
            ui.label(label);
            ui.button(format!("{selected} ▾"))
        })
        .inner;
    if button.clicked() {
        state.open_picker = if state.open_picker.as_deref() == Some(key.as_str()) {
            None
        } else {
            Some(key.clone())
        };
    }
    if state.open_picker.as_deref() != Some(key.as_str()) {
        return false;
    }

    // ComboBox closes its popup for every click outside the button, including
    // clicks in its own TextEdit. Manage the foreground Area explicitly so
    // entering search never destroys the list or its keyboard focus.
    let popup_id = ui.make_persistent_id(("designer_item_picker", &key));
    let mut close = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let popup = egui::Area::new(popup_id)
        .order(egui::Order::Foreground)
        .fixed_pos(button.rect.left_bottom())
        .constrain(true)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(button.rect.width().clamp(280., 440.));
                let query = {
                    let search = state.searches.entry(key.clone()).or_default();
                    ui.label("Browse all, or search by name or ID");
                    ui.add(
                        egui::TextEdit::singleline(search)
                            .hint_text("Name or ID")
                            .desired_width(f32::INFINITY),
                    );
                    search.trim().to_lowercase()
                };
                if let Some((id, text)) = zero {
                    if ui.selectable_label(current == id, text).clicked() {
                        if current != id {
                            set(id);
                            changed = true;
                        }
                        close = true;
                    }
                    ui.separator();
                }
                let matches = options(&query);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .max_height(320.)
                    .show(ui, |ui| {
                        if matches.is_empty() {
                            ui.label("No matching items");
                        }
                        for (id, text) in matches {
                            if ui.selectable_label(current == id, text).clicked() {
                                if current != id {
                                    set(id);
                                    changed = true;
                                }
                                close = true;
                            }
                        }
                    });
            });
        });
    let clicked_outside = ui.ctx().input(|input| {
        input.pointer.any_pressed()
            && input.pointer.interact_pos().is_some_and(|position| {
                !popup.response.rect.contains(position) && !button.rect.contains(position)
            })
    });
    if close || clicked_outside {
        state.open_picker = None;
    }
    changed
}

fn texture_combo(
    ui: &mut egui::Ui,
    state: &mut ControlState,
    catalog: &Catalog,
    design: &mut CharacterDesign,
    part: u32,
    label: &str,
    field: FaceId,
) -> bool {
    let class = design.class;
    let gender = design.gender;
    let face = &mut design.customize.face;
    let id = match field {
        FaceId::Face => &mut face.id_face_tex,
        FaceId::EyeBase => &mut face.id_eye_base_tex,
        FaceId::EyeHigh => &mut face.id_eye_high_tex,
        FaceId::Eyeball => &mut face.id_eye_ball_tex,
        FaceId::Brow => &mut face.id_brow_tex,
        FaceId::Nose => &mut face.id_nose_tex,
        FaceId::Mouth => &mut face.id_mouth_tex,
        FaceId::Hair => &mut face.id_hair_tex,
        FaceId::Goatee => &mut face.id_goatee_tex,
        FaceId::Faling => &mut face.id_faling_tex,
        FaceId::Moustache => &mut face.id_moustache_tex,
    };
    let current = *id as u32;
    let selected = catalog.face_texture(current).map_or_else(
        || format!("{current} (not in v27)"),
        |item| format!("{} — {}", item.id, item.name),
    );
    item_combo(
        ui,
        state,
        label.into(),
        current,
        selected,
        None,
        |query| {
            catalog
                .texture_options(class, gender, part)
                .into_iter()
                .filter(|item| item_matches(item.id, &item.name, query))
                .map(|item| (item.id, format!("{} — {}", item.id, item.name)))
                .collect()
        },
        |new| *id = new as u16,
    )
}

fn shape_combo(
    ui: &mut egui::Ui,
    state: &mut ControlState,
    catalog: &Catalog,
    design: &mut CharacterDesign,
    part: u32,
    label: &str,
    which: usize,
) -> bool {
    let class = design.class;
    let gender = design.gender;
    let face = &mut design.customize.face;
    let id = match which {
        0 => &mut face.id_face_shape1,
        1 => &mut face.id_face_shape2,
        2 => &mut face.id_eye_shape,
        3 => &mut face.id_brow_shape,
        4 => &mut face.id_nose_tip_shape,
        5 => &mut face.id_nose_bridge_shape,
        6 => &mut face.id_mouth_up_lip_line,
        7 => &mut face.id_mouth_mid_lip_line,
        8 => &mut face.id_mouth_down_lip_line,
        _ => &mut face.id_ear_shape,
    };
    let current = *id as u32;
    let selected = catalog.face_shape(current).map_or_else(
        || format!("{current} (not in v27)"),
        |item| format!("{} — {}", item.id, item.name),
    );
    item_combo(
        ui,
        state,
        label.into(),
        current,
        selected,
        None,
        |query| {
            catalog
                .shape_options(class, gender, part)
                .into_iter()
                .filter(|item| item_matches(item.id, &item.name, query))
                .map(|item| (item.id, format!("{} — {}", item.id, item.name)))
                .collect()
        },
        |new| *id = new as u16,
    )
}

fn hair_combo(
    ui: &mut egui::Ui,
    state: &mut ControlState,
    catalog: &Catalog,
    design: &mut CharacterDesign,
) -> bool {
    let class = design.class;
    let gender = design.gender;
    let id = &mut design.customize.face.id_hair_model;
    let current = *id as u32;
    let selected = catalog.hair_item(current).map_or_else(
        || format!("{current} (not in v27)"),
        |item| format!("{} — {}", item.id, item.name),
    );
    item_combo(
        ui,
        state,
        "Hair model".into(),
        current,
        selected,
        Some((0, "0 — bald / no additional hair skin".into())),
        |query| {
            catalog
                .hair_options(class, gender)
                .into_iter()
                .filter(|item| item_matches(item.id, &item.name, query))
                .map(|item| (item.id, format!("{} — {}", item.id, item.name)))
                .collect()
        },
        |new| *id = new as u16,
    )
}

fn weapon_combo(
    ui: &mut egui::Ui,
    state: &mut ControlState,
    catalog: &Catalog,
    design: &mut CharacterDesign,
) -> bool {
    let class = design.class;
    let id = &mut design.equipment.weapon;
    let current = *id;
    let selected = catalog.weapon(current).map_or_else(
        || format!("{current} — unarmed / not in v27"),
        |item| format!("{} — {}", item.id, item.name),
    );
    item_combo(
        ui,
        state,
        "Weapon".into(),
        current,
        selected,
        Some((0, "0 — unarmed".into())),
        |query| {
            catalog
                .weapons
                .iter()
                .filter(|item| item.class_mask & class.bit() != 0)
                .filter(|item| item_matches(item.id, &item.name, query))
                .map(|item| (item.id, format!("{} — {}", item.id, item.name)))
                .collect()
        },
        |new| *id = new,
    )
}

fn flight_combo(
    ui: &mut egui::Ui,
    state: &mut ControlState,
    catalog: &Catalog,
    design: &mut CharacterDesign,
) -> bool {
    let class = design.class;
    let id = &mut design.equipment.flight;
    let current = *id;
    let selected = catalog.flight(current).map_or_else(
        || format!("{current} — none / not in v27"),
        |item| format!("{} — {} [{}]", item.id, item.name, item.mode.name()),
    );
    item_combo(
        ui,
        state,
        "Flight item".into(),
        current,
        selected,
        Some((0, "0 — no flight item".into())),
        |query| {
            catalog
                .flight_options(class)
                .into_iter()
                .filter(|item| item_matches(item.id, &item.name, query))
                .map(|item| {
                    (
                        item.id,
                        format!(
                            "{} — {} [{} • Lv{}]",
                            item.id,
                            item.name,
                            item.mode.name(),
                            item.required_level
                        ),
                    )
                })
                .collect()
        },
        |new| *id = new,
    )
}

fn mount_combo(
    ui: &mut egui::Ui,
    state: &mut ControlState,
    catalog: &Catalog,
    design: &mut CharacterDesign,
) -> bool {
    let class = design.class;
    let id = &mut design.equipment.mount;
    let current = *id;
    let selected = catalog.mount(current).map_or_else(
        || format!("{current} — none / not a Ride pet"),
        |item| format!("{} — {}", item.id, item.name),
    );
    item_combo(
        ui,
        state,
        "Ride pet".into(),
        current,
        selected,
        Some((0, "0 — no mount".into())),
        |query| {
            catalog
                .mount_options(class)
                .into_iter()
                .filter(|item| item_matches(item.id, &item.name, query))
                .map(|item| {
                    (
                        item.id,
                        format!(
                            "{} — {} [Lv{} • {:.1} m/s]",
                            item.id, item.name, item.required_level, item.speed_a
                        ),
                    )
                })
                .collect()
        },
        |new| *id = new,
    )
}

fn armor_combo(
    ui: &mut egui::Ui,
    state: &mut ControlState,
    catalog: &Catalog,
    design: &mut CharacterDesign,
    slot: ArmorSlot,
    label: &str,
) -> bool {
    let class = design.class;
    let current = design.equipment.armor(slot);
    let native_default = design.equipment.native_default_armor(slot);
    let mut id = current;
    let selected = if current == 0 {
        if native_default {
            "0 — native profession default".to_owned()
        } else {
            "0 — empty (bare character)".to_owned()
        }
    } else {
        catalog.armor_item(current).map_or_else(
            || format!("{current} — not in v27"),
            |item| format!("{} — {}", item.id, item.name),
        )
    };
    let changed = item_combo(
        ui,
        state,
        format!("{label} armor"),
        current,
        selected,
        Some((0, "0 — empty (bare character)".into())),
        |query| {
            catalog
                .armor
                .iter()
                .filter(|item| item.slot == slot && item.class_mask & class.bit() != 0)
                .filter(|item| item_matches(item.id, &item.name, query))
                .map(|item| (item.id, format!("{} — {}", item.id, item.name)))
                .collect()
        },
        |new| id = new,
    );
    if changed {
        design.equipment.set_armor(slot, id);
    }
    changed
}

fn fashion_combo(
    ui: &mut egui::Ui,
    state: &mut ControlState,
    catalog: &Catalog,
    design: &mut CharacterDesign,
    slot: ArmorSlot,
    label: &str,
) -> bool {
    let gender = design.gender;
    let current = design.equipment.fashion(slot);
    let mut id = current;
    let selected = if current == 0 {
        "0 — empty (bare character)".to_owned()
    } else {
        catalog.fashion_item(current).map_or_else(
            || format!("{current} — not in v27"),
            |item| format!("{} — {}", item.id, item.name),
        )
    };
    let changed = item_combo(
        ui,
        state,
        format!("{label} fashion"),
        current,
        selected,
        Some((0, "0 — empty (bare character)".into())),
        |query| {
            catalog
                .fashion
                .iter()
                .filter(|item| item.slot == slot && item.gender == gender as u32)
                .filter(|item| item_matches(item.id, &item.name, query))
                .map(|item| (item.id, format!("{} — {}", item.id, item.name)))
                .collect()
        },
        |new| id = new,
    );
    design.equipment.set_fashion(slot, id);
    changed
}

fn color(ui: &mut egui::Ui, label: &str, argb: &mut u32) -> bool {
    let mut rgba = [
        ((*argb >> 16) & 255) as f32 / 255.,
        ((*argb >> 8) & 255) as f32 / 255.,
        (*argb & 255) as f32 / 255.,
        ((*argb >> 24) & 255) as f32 / 255.,
    ];
    let changed = ui
        .horizontal(|ui| {
            ui.label(label);
            ui.color_edit_button_rgba_unmultiplied(&mut rgba)
        })
        .inner
        .changed();
    if changed {
        let c = rgba.map(|value| (value.clamp(0., 1.) * 255.).round() as u32);
        *argb = (c[3] << 24) | (c[0] << 16) | (c[1] << 8) | c[2];
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::item_matches;

    #[test]
    fn item_search_matches_case_insensitive_names_ids_and_native_text() {
        assert!(item_matches(19, "☆Bronze Sword", "bronze"));
        assert!(item_matches(19, "☆Bronze Sword", "19"));
        assert!(item_matches(19, "☆Bronze Sword", ""));
        assert!(item_matches(19, "青铜剑", "青铜"));
        assert!(!item_matches(19, "☆Bronze Sword", "axe"));
    }
}

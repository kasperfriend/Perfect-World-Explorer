//! Experimental dry, opaque Hardware transport. No policy applies implicitly
//! to Reference, progressive, multi-sampled or SDK frames.

fn distance(value: Option<&str>, near: f32) -> Option<f32> {
    match value {
        None | Some("near") => Some(near),
        Some("off") => Some(0.),
        Some("full") => Some(1.0e7),
        _ => None,
    }
}

pub(super) fn requested() -> Option<[f32; 4]> {
    if std::env::var("PW_RAY_GAME_PROFILE").as_deref() != Ok("1") {
        return None;
    }
    let tier = |key, near| distance(std::env::var(key).ok().as_deref(), near);
    match (
        tier("PW_RAY_EFFECT_REFLECTION", 96.),
        tier("PW_RAY_EFFECT_SUN", 120.),
        tier("PW_RAY_EFFECT_BOUNCE", 32.),
    ) {
        (Some(r), Some(s), Some(b)) => Some([r, s, b, 1.]),
        _ => {
            eprintln!("Lighter game profile disabled: effect tiers must be off, near or full");
            None
        }
    }
}

pub(super) fn blue_noise_requested() -> bool {
    std::env::var("PW_RAY_BLUE_NOISE").as_deref() == Ok("1")
}

/// Inject only into the opt-in Vulkan sibling. Keep normal/Reference WGSL
/// unchanged apart from the shared uniform's defaulted tail. The anchors
/// are exact strings: do not silently turn the requested profile into full RT.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(super) fn specialize(mut source: String, noise: bool) -> Result<String, String> {
    for (anchor, text) in [
        (
            "fn direct(p:",
            include_str!("game_profile.wgsl"),
        ),
        (
            "        var tr = vec3<f32>(0.);\n        if cache && (sun_cache_read",
            "        if game_opaque(s, medium, purpose) && (!sun_leg_selected || \
               frame.ray_effects.y<=0. || t>frame.ray_effects.y) {\n\
               color += response*light.energy; continue;\n        }\n\
               if game_opaque(s, medium, purpose) { light.distance = min(light.distance,frame.ray_effects.y); }\n",
        ),
        (
            "            if physical_material(s) && (frame.options.x & LIGHTING)!=0u {",
            include_str!("game_transport.wgsl"),
        ),
    ] {
        let offset = source.find(anchor).ok_or("missing Lighter game profile specialization site")?;
        source.insert_str(offset, text);
    }
    source = source
        .replacen("        if sun_pending {\n            sun_pending = false;", "        let sun_leg_selected = sun_pending;\n        if sun_pending {\n            sun_pending = false;", 1)
        .replacen("terminal || sheet,medium.body,path.purpose", "terminal || sheet || game_opaque(s,medium.body,path.purpose),medium.body,path.purpose", 1);
    if noise {
        source.push_str(include_str!(concat!(
            env!("OUT_DIR"),
            "/lighter-blue-noise.wgsl"
        )));
        source.push_str(include_str!("game_noise.wgsl"));
        source = source
            .replacen("    let delta = u.x*0.000011103284;", "    if frame.ray_effects.w>0. && purpose==VIEW { u = game_noise(ray, 0u); }\n    let delta = u.x*0.000011103284;", 1)
            .replace("let d = hemisphere(s.n,seed);\n                    if dot(d,s.face)>0. {", "let u = game_noise(ray, 7u);\n                    let d = around(s.n,sqrt(1.-u.x),sqrt(u.x),u.y*6.2831853);\n                    if dot(d,s.face)>0. {");
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_profile_rejects_unknown_effect_tier_and_keeps_off_at_zero() {
        assert_eq!(distance(None, 32.), Some(32.));
        assert_eq!(distance(Some("near"), 96.), Some(96.));
        assert_eq!(distance(Some("off"), 96.), Some(0.));
        assert_eq!(distance(Some("full"), 96.), Some(1.0e7));
        assert_eq!(distance(Some("99"), 96.), None);
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn game_profile_shader_lowers_both_vulkan_entries() {
        use super::super::{trace_source, vulkan, HARDWARE};
        for noise in [false, true] {
            let code = specialize(trace_source(HARDWARE, false, false), noise).unwrap();
            assert!(vulkan::shader(&code).unwrap().len() > 64);
            assert!(
                vulkan::shader_entry(&code, naga_rt::ShaderStage::Compute, "render_hybrid")
                    .unwrap()
                    .len()
                    > 64
            );
        }
    }
}

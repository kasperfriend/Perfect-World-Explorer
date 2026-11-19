//! Dependency/light-presence audit only. This does not execute GFX controllers.
use std::collections::BTreeMap;
#[derive(Debug)]
pub struct Audit {
    pub version: u32,
    pub elements: BTreeMap<u32, usize>,
    pub children: Vec<String>,
    pub lights: usize,
}
pub fn parse(text: &str) -> Result<Audit, String> {
    let mut lines = text
        .strip_prefix("MOXT")
        .ok_or("expected text GFX signature")?
        .lines();
    let version = lines
        .next()
        .and_then(|l| l.trim().strip_prefix("Version: "))
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or("invalid GFX version")?;
    // No version cap. An audit is a dependency census over the `Key: value`
    // lines, and `A3DGFXLight::Load` reads the same fourteen light lines at
    // every version, so a 90 or 93 root audits exactly like a 33 one.
    if version == 0 {
        return Err("invalid GFX version for light audit".into());
    }
    let mut out = Audit {
        version,
        elements: BTreeMap::new(),
        children: Vec::new(),
        lights: 0,
    };
    let mut expected = None;
    let mut current = None;
    let mut light_types = 0;
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "GFXELEMENTCOUNT" => {
                if expected.is_some() {
                    return Err("duplicate GFX element count".into());
                }
                let n = value
                    .parse::<usize>()
                    .map_err(|_| "invalid GFX element count")?;
                if n > 100_000 {
                    return Err("GFX element count exceeds limit".into());
                }
                expected = Some(n);
            }
            "GFXELEMENTID" => {
                if current == Some(130) && light_types != 1 {
                    return Err("light element missing type".into());
                }
                let id = value.parse::<u32>().map_err(|_| "invalid GFX element ID")?;
                *out.elements.entry(id).or_default() += 1;
                current = Some(id);
                light_types = 0;
                if id == 130 {
                    out.lights += 1;
                }
            }
            "LightType" => {
                if current != Some(130) || value != "1" || light_types != 0 {
                    return Err("unsupported or misplaced GFX light type".into());
                }
                light_types += 1;
            }
            "GfxPath" => {
                if current != Some(200)
                    || value.is_empty()
                    || !value.to_ascii_lowercase().ends_with(".gfx")
                {
                    return Err("unsupported GFX container reference".into());
                }
                out.children.push(value.into());
            }
            _ => {
                if value.to_ascii_lowercase().ends_with(".gfx") {
                    return Err("unclassified GFX dependency field".into());
                }
            }
        }
    }
    if current == Some(130) && light_types != 1 {
        return Err("light element missing type".into());
    }
    if expected != Some(out.elements.values().sum()) {
        return Err("GFX element count mismatch".into());
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifies_nested_dependencies_without_claiming_playback() {
        let a=parse("MOXTVersion: 37\r\nGFXELEMENTCOUNT: 3\r\nGFXELEMENTID: 100\r\nGFXELEMENTID: 200\r\nGfxPath: scene\\child.gfx\r\nGFXELEMENTID: 130\r\nLightType: 1\r\n").unwrap();
        assert_eq!(a.version, 37);
        assert_eq!(a.lights, 1);
        assert_eq!(a.children, ["scene\\child.gfx"]);
        assert_eq!(a.elements[&200], 1);
    }
    #[test]
    fn rejects_incomplete_counts_unknown_formats_and_unclassified_edges() {
        for s in [
            "",
            "MOXBVersion: 37",
            "MOXTVersion: 80\nGFXELEMENTCOUNT: 0",
            "MOXTVersion: 37\nGFXELEMENTID: 100",
            "MOXTVersion: 37\nGFXELEMENTCOUNT: 1",
            "MOXTVersion: 37\nGFXELEMENTCOUNT: 0\nGFXELEMENTCOUNT: 0",
            "MOXTVersion: 37\nGFXELEMENTCOUNT: 1\nGFXELEMENTID: 100\nOtherFile: child.gfx",
            "MOXTVersion: 37\nGFXELEMENTCOUNT: 1\nGFXELEMENTID: 100\nGfxPath: child.gfx",
        ] {
            assert!(parse(s).is_err(), "{s}");
        }
    }
    #[test]
    fn cannot_hide_or_misclassify_a_light_record() {
        for s in [
            "GFXELEMENTID: 130",
            "GFXELEMENTID: 130\nLightType: 2",
            "GFXELEMENTID: 100\nLightType: 1",
            "GFXELEMENTID: 130\nLightType: 1\nLightType: 1",
        ] {
            assert!(parse(&format!("MOXTVersion: 37\nGFXELEMENTCOUNT: 1\n{s}")).is_err());
        }
        assert_eq!(
            parse("MOXTVersion: 79\nGFXELEMENTCOUNT: 0").unwrap().lights,
            0
        );
    }
}

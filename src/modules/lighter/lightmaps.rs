//! Optional legacy baked light for streamed terrain and grass. Keep the day
//! and night references separate so a day-only fallback cannot shade grass.
use crate::{
    material::Materials,
    modules::lighter::textures::{chain_bytes, Filter, Textures},
    terrain_lightmaps::{Availability, Layout, Pair, Tiles, TILES},
    Terrain,
};

const BUDGET: usize = 12 << 20;

pub(super) fn load(
    t: &Terrain,
    material: &Materials,
    bank: &mut Textures,
) -> Result<Vec<[u32; 2]>, String> {
    let paths = |name: &str| {
        [
            t.map_dir.join(format!("{name}.t2lm")),
            t.map_dir.join(format!("{name}.t2lm1")),
        ]
    };
    if !t
        .subterrain_names
        .iter()
        .any(|name| paths(name)[0].is_file())
    {
        return Ok(Vec::new());
    }
    let layout = Layout::new(
        material.areas_x,
        material.areas.len(),
        t.subterrain_names.len(),
    )?;
    let mut side = 64;
    while side > 1 && chain_bytes([256; 2], side).saturating_mul(material.areas.len() * 2) > BUDGET
    {
        side /= 2;
    }
    let start = bank.byte_len();
    let mut offsets = vec![[0; 2]; material.areas.len()];
    let mut missing = 0;
    for (sub, name) in t.subterrain_names.iter().enumerate() {
        let [day_path, night_path] = paths(name);
        let load_pair = || -> Result<_, String> {
            let Some(pair) = Pair::read_optional(&day_path, &night_path)? else {
                return Ok(None);
            };
            Tiles::parse(pair.bytes(false)).map_err(|e| format!("{}: {e}", day_path.display()))?;
            if pair.availability() == Availability::Both {
                Tiles::parse(pair.bytes(true))
                    .map_err(|e| format!("{}: {e}", night_path.display()))?;
            }
            Ok(Some(pair))
        };
        let pair = match load_pair() {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                missing += TILES;
                continue;
            }
            Err(error) => {
                eprintln!("Lighter: baked lightmap pair unbound ({error})");
                missing += TILES;
                continue;
            }
        };
        let day = Tiles::parse(pair.bytes(false))?;
        let night = if pair.availability() == Availability::Both {
            Some(Tiles::parse(pair.bytes(true))?)
        } else {
            None
        };
        for tile in 0..TILES {
            let at = layout.area(sub, tile).unwrap();
            let mut stage = |raw| -> Result<u32, String> {
                let (levels, _) = crate::model_texture_gpu::decode_for_rays(raw)?;
                let tile =
                    crate::modules::lighter::textures::Tile::images(&levels, side, Filter::Data)?;
                if bank.byte_len() - start + tile.bytes() > BUDGET {
                    return Err("Lighter baked-lightmap budget exhausted".into());
                }
                bank.insert(&tile)
            };
            let d = stage(day.tile(tile).unwrap())?;
            let n = night
                .as_ref()
                .map(|map| stage(map.tile(tile).unwrap()))
                .transpose()?
                .unwrap_or(d);
            offsets[at] = [d, n];
        }
    }
    eprintln!("Lighter: baked lightmaps at up to {side} pixels ({} bound, {missing} unbound areas, {} KiB)", offsets.len() - missing, (bank.byte_len() - start) >> 10);
    Ok(offsets)
}

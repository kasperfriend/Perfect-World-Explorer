//! The same sky triples as the native renderer, sampled by world-space miss
//! directions (including reflected rays). No raster sky capture is needed.
use super::textures::{Filter, Textures};
use std::{collections::HashMap, fs::File, path::Path};

#[derive(Default)]
pub(crate) struct Sky {
    sets: [[u32; 3]; 3],
}

impl Sky {
    pub fn load(element: &Path, bank: &mut Textures) -> Result<Self, String> {
        let mut file = File::open(element.join("textures.pck")).map_err(|e| e.to_string())?;
        let entries: HashMap<_, _> = crate::pck::directory(&mut file)?
            .into_iter()
            .map(|e| (e.name.to_lowercase(), e))
            .collect();
        let mut offsets = HashMap::new();
        let mut read = |path: &str, i: usize| -> Result<u32, String> {
            let key = crate::sky_gpu::sky_pack_key(path);
            let key = if entries.contains_key(&key) {
                key
            } else {
                crate::sky_gpu::sky_pack_key(crate::sky_gpu::SKY_DEFAULTS[[1, 2, 0][i]])
            };
            if let Some(&offset) = offsets.get(&key) {
                return Ok(offset);
            }
            let entry = entries
                .get(&key)
                .ok_or_else(|| format!("missing sky tile {key}"))?;
            let bytes = crate::pck::read_entry(&mut file, entry)?;
            let image = image::load_from_memory(&bytes)
                .map_err(|e| format!("{key}: {e}"))?
                .to_rgba8();
            let offset = bank.rgba(
                [image.width(), image.height()],
                image.as_raw(),
                Filter::Color,
            )?;
            offsets.insert(key, offset);
            Ok(offset)
        };
        let selection = crate::sky_gpu::sky_selection_from_configs(element);
        let default = crate::sky_gpu::read_sky_images(element);
        let mut result = Self::default();
        for (i, triple) in result.sets.iter_mut().enumerate() {
            let paths = selection
                .as_ref()
                .and_then(|s| s.sets.get(i))
                .map_or([&*default[1], &*default[2], &*default[0]], |s| {
                    [&*s.panorama_01, &*s.panorama_02, &*s.roof]
                });
            for (j, path) in paths.into_iter().enumerate() {
                triple[j] = read(path, j)?;
            }
        }
        Ok(result)
    }

    pub fn layers(&self, set: i32, payload: u32) -> [u32; 4] {
        let [front, back, roof] = self.sets[set.clamp(0, 2) as usize];
        [front, back, roof, payload]
    }
}

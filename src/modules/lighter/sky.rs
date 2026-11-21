//! The same sky triples as the native renderer, sampled by world-space miss
//! directions (including reflected rays). No raster sky capture is needed.
use super::textures::{Filter, Textures, COLOR_SIDE};
use glam::Vec3;
use std::{collections::HashMap, fs::File, path::Path};

#[derive(Default)]
pub(crate) struct Sky {
    sets: [[u32; 3]; 3],
    cdfs: [u32; 3],
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
            let (images, _) = crate::model_texture_gpu::decode_for_rays(&bytes)
                .map_err(|e| format!("{key}: {e}"))?;
            let offset = bank.images(&images, COLOR_SIDE, Filter::Color)?;
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
        for i in 0..3 {
            result.cdfs[i] = if let Some(j) = (0..i).find(|&j| result.sets[i] == result.sets[j]) {
                result.cdfs[j]
            } else {
                pack_cdf(bank, result.sets[i])?
            };
        }
        Ok(result)
    }

    pub fn distributions(&self, current: i32, next: i32) -> [u32; 4] {
        [
            self.cdfs[current.clamp(0, 2) as usize],
            self.cdfs[next.clamp(0, 2) as usize],
            0,
            0,
        ]
    }

    pub fn layers(&self, set: i32, payload: u32) -> [u32; 4] {
        let [front, back, roof] = self.sets[set.clamp(0, 2) as usize];
        [front, back, roof, payload]
    }
}

// Equal solid angle bins: azimuth vs. uniform y, not polar angle.
// Must match reference.wgsl. A 1% uniform mix retains support for the moon,
// sky-set crossfades and a drifting roof even in nominally black cells.
const WIDTH: usize = 64;
const HEIGHT: usize = 32;

pub(super) fn pack_cdf(bank: &mut Textures, tiles: [u32; 3]) -> Result<u32, String> {
    let radiance = sky_cells(|d| proposal_radiance(bank, tiles, d));
    let mut words = sky_cdf(&radiance);
    words.extend(radiance.iter().flat_map(|r| r.to_array().map(f32::to_bits)));
    let addr = bank.reserve(words.len())?;
    bank.write_words(addr, &words);
    Ok(addr as u32)
}

fn sky_cells(radiance: impl Fn(Vec3) -> Vec3) -> Vec<Vec3> {
    (0..WIDTH * HEIGHT)
        .map(|i| {
            let y = 2. * ((i / WIDTH) as f32 + 0.5) / HEIGHT as f32 - 1.;
            let phi = std::f32::consts::TAU * ((i % WIDTH) as f32 + 0.5) / WIDTH as f32;
            let r = (1. - y * y).sqrt();
            radiance(Vec3::new(r * phi.cos(), y, r * phi.sin()))
        })
        .collect()
}

fn sky_cdf(radiance: &[Vec3]) -> Vec<u32> {
    let mass: Vec<_> = radiance
        .iter()
        .map(|r| f64::from(r.dot(Vec3::new(0.2126, 0.7152, 0.0722)).max(0.)))
        .collect();
    let sum = mass.iter().sum::<f64>();
    let uniform = 1. / mass.len() as f64;
    let mut acc = 0.;
    let mut words: Vec<_> = mass
        .into_iter()
        .map(|p| {
            acc += if sum > 0. {
                0.99 * p / sum + 0.01 * uniform
            } else {
                uniform
            };
            (acc as f32).to_bits()
        })
        .collect();
    *words.last_mut().unwrap() = 1.0f32.to_bits();
    words
}

fn proposal_radiance(bank: &Textures, tiles: [u32; 3], d: Vec3) -> Vec3 {
    // Low-cost preview of sky_triple without UV drift or trilinear filtering.
    let y = d.y.max(0.);
    let cap = (-47.685_85 * y + (226.059_95 + 2273.94 * y * y).sqrt()) / 30.070_58;
    let scale = crate::sky_gpu::SKY_DRIFT_SCALE;
    let roof = bank.sky_texel(
        tiles[2],
        [(0.5 + d.x * cap) * scale, (0.5 + d.z * cap) * scale],
        true,
    );
    let az = d.z.abs().atan2(d.x);
    let pi = std::f32::consts::PI;
    let u = (if az >= pi * 0.5 { 2. } else { 1. }) - 2. * az / pi;
    let v = 0.275 + 0.225 * ((50. / -10.602_875) * d.y / d.x.hypot(d.z).max(1.0e-6)).clamp(0., 1.);
    let skirt = bank.sky_texel(tiles[usize::from(d.z < 0.)], [u, v], false);
    let t = ((d.y + 0.015) / 0.03).clamp(0., 1.);
    skirt.lerp(roof, t * t * (3. - 2. * t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_retains_support_and_weights_power() {
        let cdf: Vec<_> = sky_cdf(&sky_cells(|dir| {
            Vec3::splat(if dir.y > 0. { 9. } else { 1. })
        }))
        .into_iter()
        .map(f32::from_bits)
        .collect();
        assert!(cdf[0] > 0. && cdf.windows(2).all(|w| w[1] > w[0]));
        assert!((cdf[WIDTH * HEIGHT / 2 - 1] - 0.104).abs() < 1.0e-6);
        assert_eq!(*cdf.last().unwrap(), 1.);
        let black = sky_cdf(&sky_cells(|_| Vec3::ZERO));
        assert_eq!(f32::from_bits(black[WIDTH * HEIGHT / 2 - 1]), 0.5);
    }
}

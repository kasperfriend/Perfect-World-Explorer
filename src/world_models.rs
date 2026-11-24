//! Retain ECWLD model placements and shared model dependencies. Loading an
//! asset does not choose an action namespace or invent an animation schedule.
use crate::{ecmodel, model_asset::ModelAsset, scene::SceneModel};
use std::{collections::HashMap, fs::File, path::Path};

pub struct Instance {
    pub placement: SceneModel,
    pub asset: usize,
    /// Effective combined action after the source idle fallback; None is
    /// a non-playing model, not a direct-SMD-name dispatch.
    pub action: Option<usize>,
    pub playback: Result<crate::model_playback::Playback, String>,
}
#[derive(Default)]
pub struct Assets {
    pub assets: Vec<ModelAsset>,
    pub instances: Vec<Instance>,
    pub failures: Vec<String>,
    pub unloaded: Vec<SceneModel>,
}
impl Assets {
    /// Failed dependencies remain visible as named failures, not substituted
    /// meshes. A failed shared asset is read once, not retried per placement.
    pub fn load_with(
        placements: Vec<SceneModel>,
        mut read: impl FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> Self {
        let mut result = Self {
            assets: vec![],
            instances: vec![],
            failures: vec![],
            unloaded: vec![],
        };
        let mut cache: HashMap<String, Option<usize>> = HashMap::new();
        let mut tracks = crate::model_asset::TrackCache::default();
        result.instances.reserve(placements.len());
        for placement in placements {
            let path = ecmodel::normalize(&placement.path);
            let asset = if let Some(index) = cache.get(&path) {
                *index
            } else {
                let index = match ModelAsset::load_cached(&path, &mut read, &mut tracks) {
                    Ok(asset) => {
                        let index = result.assets.len();
                        result.assets.push(asset);
                        Some(index)
                    }
                    Err(e) => {
                        result.failures.push(e);
                        None
                    }
                };
                cache.insert(path, index);
                index
            };
            if let Some(asset) = asset {
                let action = result.assets[asset]
                    .descriptor
                    .scene_action(&placement.action);
                let playback = crate::model_playback::Playback::new(&result.assets[asset], action);
                result.instances.push(Instance {
                    placement,
                    asset,
                    action,
                    playback,
                });
            } else {
                result.unloaded.push(placement);
            }
        }
        result
    }
    pub fn open(element: &Path, placements: Vec<SceneModel>) -> Result<Self, String> {
        if placements.is_empty() {
            return Ok(Self::load_with(placements, |_| unreachable!()));
        }
        let mut file =
            File::open(element.join("models.pck")).map_err(|e| format!("models.pck: {e}"))?;
        let dir = crate::pck::directory_arc(&mut file)?;
        let mut entries: HashMap<String, usize> = HashMap::with_capacity(dir.len());
        for (idx, entry) in dir.iter().enumerate() {
            let name = ecmodel::normalize(&entry.name);
            if entries.insert(name.clone(), idx).is_some() {
                return Err(format!("duplicate normalized models.pck entry: {name}"));
            }
        }
        // Deduplicate unique normalized ECM paths in first-seen order so we
        // can load & parse all unique world models in parallel across all CPU
        // cores while preserving exact deterministic asset/failure order.
        let mut unique_paths: Vec<String> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for placement in &placements {
            let path = ecmodel::normalize(&placement.path);
            if !seen.contains_key(&path) {
                let idx = unique_paths.len();
                seen.insert(path.clone(), idx);
                unique_paths.push(path);
            }
        }
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(32)
            .min(unique_paths.len().max(1));
        let loaded: Vec<Result<ModelAsset, String>> = if workers > 1 && unique_paths.len() >= 2 {
            let chunk_size = unique_paths.len().div_ceil(workers);
            let file_ref = &file;
            let dir_ref = &dir;
            let entries_ref = &entries;
            std::thread::scope(|s| {
                let handles: Vec<_> = unique_paths
                    .chunks(chunk_size)
                    .map(|chunk| {
                        s.spawn(move || {
                            let mut tracks = crate::model_asset::TrackCache::default();
                            let mut read = |path: &str| -> Result<Vec<u8>, String> {
                                let &idx = entries_ref
                                    .get(path)
                                    .ok_or_else(|| format!("missing models.pck entry: {path}"))?;
                                crate::pck::read_entry_at(file_ref, &dir_ref[idx])
                                    .map_err(|e| e.to_string())
                            };
                            chunk
                                .iter()
                                .map(|path| ModelAsset::load_cached(path, &mut read, &mut tracks))
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|h| h.join().unwrap())
                    .collect()
            })
        } else {
            let mut tracks = crate::model_asset::TrackCache::default();
            let mut read = |path: &str| -> Result<Vec<u8>, String> {
                let &idx = entries
                    .get(path)
                    .ok_or_else(|| format!("missing models.pck entry: {path}"))?;
                crate::pck::read_entry_at(&file, &dir[idx]).map_err(|e| e.to_string())
            };
            unique_paths
                .iter()
                .map(|path| ModelAsset::load_cached(path, &mut read, &mut tracks))
                .collect()
        };
        let mut result = Self {
            assets: Vec::with_capacity(unique_paths.len()),
            instances: Vec::with_capacity(placements.len()),
            failures: Vec::new(),
            unloaded: Vec::new(),
        };
        let mut slot_map: Vec<Option<usize>> = Vec::with_capacity(loaded.len());
        for outcome in loaded {
            match outcome {
                Ok(asset) => {
                    let index = result.assets.len();
                    result.assets.push(asset);
                    slot_map.push(Some(index));
                }
                Err(e) => {
                    result.failures.push(e);
                    slot_map.push(None);
                }
            }
        }
        for placement in placements {
            let path = ecmodel::normalize(&placement.path);
            let asset = seen.get(&path).and_then(|&u| slot_map[u]);
            if let Some(asset) = asset {
                let action = result.assets[asset]
                    .descriptor
                    .scene_action(&placement.action);
                let playback = crate::model_playback::Playback::new(&result.assets[asset], action);
                result.instances.push(Instance {
                    placement,
                    asset,
                    action,
                    playback,
                });
            } else {
                result.unloaded.push(placement);
            }
        }
        Ok(result)
    }
}
/// 1.4.2 0x6d4304 -> 0x7b0d60 -> 0x788960. Normalize forward/up
/// separately, then their cross product; do NOT silently orthogonalize up.
pub fn transform(placement: &SceneModel) -> Result<glam::Mat4, String> {
    let position = glam::Vec3::from_array(placement.position);
    let direction = glam::Vec3::from_array(placement.direction);
    let up = glam::Vec3::from_array(placement.up);
    if !position.is_finite()
        || !direction.is_finite()
        || !up.is_finite()
        || direction.length_squared() < 1e-12
        || up.length_squared() < 1e-12
    {
        return Err("invalid world model placement basis".into());
    }
    let direction = direction.normalize();
    let up = up.normalize();
    let right = up.cross(direction);
    if right.length_squared() < 1e-12 {
        return Err("parallel world model placement axes".into());
    }
    Ok(glam::Mat4::from_cols(
        right.normalize().extend(0.),
        up.extend(0.),
        direction.extend(0.),
        position.extend(1.),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_basis_normalizes_axes_but_preserves_nonorthogonal_up() {
        let p = SceneModel {
            export_id: 1,
            position: [10., 2., -3.],
            direction: [0., 0., 2.],
            up: [0., 2., 2.],
            path: String::new(),
            action: String::new(),
        };
        let m = transform(&p).unwrap();
        assert_eq!(m.w_axis.to_array(), [10., 2., -3., 1.]);
        for (value, expected) in m.x_axis.to_array().into_iter().zip([1., 0., 0., 0.]) {
            assert!((value - expected).abs() < 1e-6);
        }
        assert_eq!(m.z_axis.to_array(), [0., 0., 1., 0.]);
        assert!((m.y_axis.z - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        let mut p = p;
        p.up = p.direction;
        assert!(transform(&p).is_err());
        p.up = [0., 1., 0.];
        p.position[0] = f32::NAN;
        assert!(transform(&p).is_err());
    }
    #[test]
    fn failed_shared_assets_are_read_once_and_named() {
        let mut reads = 0;
        let placement = SceneModel {
            export_id: 7,
            position: [0.; 3],
            direction: [0., 0., 1.],
            up: [0., 1., 0.],
            path: "Models\\missing.ecm".into(),
            action: "visible".into(),
        };
        let assets = Assets::load_with(vec![placement.clone(), placement], |path| {
            reads += 1;
            assert_eq!(path, "models/missing.ecm");
            Err("missing dependency".into())
        });
        assert_eq!(reads, 1);
        assert!(assets.instances.is_empty());
        assert_eq!(assets.failures.len(), 1);
        assert!(assets.failures[0].contains("models/missing.ecm: missing dependency"));
    }
}

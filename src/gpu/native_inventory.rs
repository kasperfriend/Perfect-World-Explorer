//! Headless Native main-stream accounting. A draw admitted here can still be
//! occluded, clipped or alpha-tested. No diagnostic readbacks in timed draws.
use super::{skip_trees_test, ui_test, Camera, MeshData, Renderer, Terrain, TILE};
use crate::{
    modules::lighter::coverage::{Class, Counts, Inventory},
    terrain_residency::Span,
    world_models_gpu::NativeModelDraw,
    world_objects::{aabb_in_frustum, frustum_planes_from_matrix},
};
use glam::Mat4;
use std::{collections::HashMap, fmt::Write};

const SCOPE: &str = "main_streamed_v1";

pub(super) struct Capture {
    inventory: Inventory,
    grass_areas: Counts,
    grass_mask_cells: usize,
    expected: [usize; 5],
}

pub(super) fn capture_scene(t: &Terrain, m: &MeshData, vp: Mat4) -> Capture {
    let mut inventory = Inventory::default();
    let frustum = frustum_planes_from_matrix(vp.to_cols_array());
    let bounds = m.detail.terrain_cutoff.then(|| {
        let w = m.lod.window;
        crate::terrain_cutoff::world_bounds(
            [w.x0, w.y0, w.x1, w.y1],
            TILE,
            t.width,
            t.height,
            t.grid_spacing,
        )
    });
    let mut present = HashMap::new();
    for page in &m.local_pages {
        let page_visible = aabb_in_frustum(&frustum, page.page_min, page.page_max);
        for (span, extent) in page.placements.iter().zip(&page.bounds) {
            if span.start >= span.end {
                continue;
            }
            // One BMD can span multiple page/material ranges; count it once.
            let visible = page_visible && aabb_in_frustum(&frustum, extent.min, extent.max);
            *present.entry(span.export_id).or_insert(false) |= visible;
        }
    }
    let row = &mut inventory[Class::Buildings];
    for (i, ornament) in t.objects.ornaments.iter().enumerate() {
        if !crate::terrain_cutoff::contains(bounds, ornament.position) {
            row.cutoff += 1;
        } else if !t.scenery_residency.resident(i) {
            row.range += 1;
        } else if let Some(model) = t.objects.get(i) {
            match present.get(&ornament.export_id) {
                Some(true) => {
                    row.loaded += 1;
                    row.texture_fallback += model
                        .meshes
                        .iter()
                        .filter(|mesh| {
                            !mesh.indices.is_empty()
                                && !mesh.texture.is_empty()
                                && t.objects.texture(&mesh.texture).is_none()
                        })
                        .count();
                }
                Some(false) => row.range += 1,
                None => row.empty += 1,
            }
        } else if t.objects.has_lit_archive() {
            row.unavailable += 1;
        } else {
            row.package += 1;
        }
    }
    let row = &mut inventory[Class::Grass];
    for batch in m.grass.geometry().3 {
        if batch.indices.is_empty() {
            row.empty += batch.instances.len();
        } else {
            row.loaded += batch.instances.len();
        }
    }
    let row = &mut inventory[Class::Water];
    *row = m.water_coverage;
    row.loaded = 0;
    for &(_, lo, hi, _) in &m.water_areas {
        // This area may have coalesced with other coplanar areas. Check the
        // submitted *batch's* bounds, not the culling a separate draw would use.
        let batch = &m.water_batches[m.water_batches.partition_point(|b| b.end <= lo)];
        debug_assert!(batch.start <= lo && hi <= batch.end);
        if ui_test() {
            row.disabled += 1;
        } else if batch.visible(vp) {
            row.loaded += 1;
        } else {
            row.range += 1;
        }
    }
    Capture {
        inventory,
        grass_areas: m.grass_area_coverage,
        grass_mask_cells: t.grass.areas.iter().map(|a| a.active.len()).sum(),
        expected: [
            t.objects.ornaments.len(),
            t.water.len(),
            t.trees.placements.len(),
            t.models.instances.len() + t.models.unloaded.len(),
            t.grass.areas.len(),
        ],
    }
}

fn terrain_draws(spans: &[Span], runs: &[(u32, u32)], source: usize) -> Counts {
    let mut row = Counts {
        range: source - spans.len(),
        ..Counts::default()
    };
    for span in spans {
        if span.count == 0 {
            row.empty += 1;
            continue;
        }
        // Adjacent visible spans are folded into a run. A range test against
        // the whole span avoids counting a new window against stale runs.
        let at = runs.partition_point(|&(first, count)| first + count <= span.start);
        if runs.get(at).is_some_and(|&(first, count)| {
            first <= span.start && span.start + span.count <= first + count
        }) {
            row.loaded += 1;
        } else {
            row.range += 1;
        }
    }
    row
}

impl Capture {
    pub(super) fn finish(
        &mut self,
        r: &Renderer,
        cam: &Camera,
        meta: &mut String,
    ) -> Result<Inventory, String> {
        let rows = &mut self.inventory;
        let blocks = (r.world_width / TILE) * (r.world_height / TILE);
        rows[Class::NearTerrain] = terrain_draws(&r.spans, &r.terrain_runs, blocks);
        let far_patches =
            (r.world_width - 1).div_ceil(4 * 32) * (r.world_height - 1).div_ceil(4 * 32);
        rows[Class::FarTerrain] = if r.detail.terrain_cutoff {
            Counts {
                cutoff: far_patches,
                ..Counts::default()
            }
        } else {
            terrain_draws(r.terrain_gpu.far_spans(), &r.far_terrain_runs, far_patches)
        };
        let trees_enabled = !ui_test() && !skip_trees_test();
        let trees = r.far_trees.placement_census(trees_enabled);
        rows[Class::Trees] = Counts {
            loaded: trees.loaded,
            cutoff: trees.cutoff,
            invalid: trees.invalid,
            empty: trees.empty,
            unavailable: trees.unavailable,
            budget: trees.budget,
            disabled: trees.disabled,
            ..Counts::default()
        };
        let lod_instances = if trees_enabled {
            r.far_trees.submitted_instances(&r.device, &r.queue)
        } else {
            0
        };
        let mut model_rows = r.ray_model_absences;
        let (mut quads, mut dest_color) = (0, 0);
        if let Some(models) = &r.models {
            for draw in models.native_model_draws() {
                match draw {
                    NativeModelDraw::Submitted => model_rows.loaded += 1,
                    NativeModelDraw::Cutoff => model_rows.cutoff += 1,
                    NativeModelDraw::Culled => model_rows.range += 1,
                    NativeModelDraw::Invalid => model_rows.invalid += 1,
                    NativeModelDraw::Empty => model_rows.empty += 1,
                }
            }
            let fx = models.native_effect_draws();
            rows[Class::EffectDraws] = Counts {
                loaded: fx.submitted,
                unavailable: fx.unavailable,
                unsupported: fx.unsupported,
                empty: fx.empty,
                texture_fallback: fx.fallback,
                ..Counts::default()
            };
            quads = models.effect_quad_count();
            dest_color = fx.dest_color_submitted;
        }
        rows[Class::WorldModels] = model_rows;
        rows[Class::EffectRoots] = r.effect_root_coverage().0;
        // Capture does not install Walker or Populator add-ons. Scene critter
        // references are reported separately by the caller, not invented as
        // active actors. Reflections/refraction are not counted as new owners.
        let [buildings, water, trees, models, grass] = self.expected;
        for (name, row, expected) in [
            ("buildings", rows[Class::Buildings], buildings),
            ("water", rows[Class::Water], water),
            ("trees", rows[Class::Trees], trees),
            ("world_models", rows[Class::WorldModels], models),
            ("grass_areas", self.grass_areas, grass),
            ("effect_draws", rows[Class::EffectDraws], quads),
        ] {
            if row.total() != expected {
                return Err(format!(
                    "Native capture {name} inventory accounts for {} of {expected}",
                    row.total()
                ));
            }
        }
        let grass_areas = self.grass_areas;
        // Generated slots before per-vertex wet-mask, clip and depth tests.
        let rain_slots = if r.frame.weather.rain && cam.underwater_depth <= 0. {
            crate::weather_gpu::RAIN_INSTANCES
        } else {
            0
        };
        writeln!(meta,
            "native_inventory_scope={SCOPE}\nnative_local_scenery_pages={}\nnative_last_scene_passes={}\nnative_effect_quads={quads}\nnative_dest_color_draws_submitted={dest_color}\nnative_visible_water_batches={}\nnative_tree_lod_instances_submitted={lod_instances}\nnative_rain_slots_selected={rain_slots}\n\
             native_authored_bmds={buildings}\nnative_authored_water_areas={water}\nnative_authored_tree_placements={trees}\nnative_authored_world_actors={models}\n\
             grass_inventory_scope=native_stream_only\ngrass_authored_areas={grass}\ngrass_authored_mask_cells={}\ngrass_areas_loaded={}\ngrass_areas_skipped={}\ngrass_areas_failed={}\ngrass_areas_range={}\ngrass_areas_empty={}\ngrass_areas_cutoff={}\ngrass_areas_budget={}\ngrass_areas_disabled={}\ngrass_areas_unavailable={}",
            r.local_pages.len(), r.scene_passes, r.visible_water_batches.len(), self.grass_mask_cells,
            grass_areas.loaded, grass_areas.skipped(), grass_areas.failed(), grass_areas.range,
            grass_areas.empty, grass_areas.cutoff, grass_areas.budget, grass_areas.disabled,
            grass_areas.unavailable,
        ).unwrap();
        Ok(*rows)
    }
}

/// Report the largest Vulkan DEVICE_LOCAL heap, not its current budget or
/// resident bytes. This distinguishes the two RTX 3080 memory capacities.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(super) fn device_local_heap(device: &wgpu::Device) -> Option<u64> {
    unsafe {
        device
            .as_hal::<wgpu::hal::api::Vulkan, _, _>(|hal| {
                let hal = hal?;
                let memory = hal
                    .shared_instance()
                    .raw_instance()
                    .get_physical_device_memory_properties(hal.raw_physical_device());
                memory.memory_heaps[..memory.memory_heap_count as usize]
                    .iter()
                    .filter(|h| h.flags.contains(ash::vk::MemoryHeapFlags::DEVICE_LOCAL))
                    .map(|h| h.size)
                    .max()
            })
            .flatten()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub(super) fn device_local_heap(_: &wgpu::Device) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_draw_runs_count_source_spans_not_calls_or_fragments() {
        let span = |start, count| Span {
            block: (0, 0),
            start,
            count,
            bounds: [0.; 6],
        };
        let counts = terrain_draws(
            &[span(0, 6), span(6, 3), span(9, 3), span(12, 0)],
            &[(0, 9)],
            5,
        );
        assert_eq!(
            (counts.loaded, counts.range, counts.empty, counts.total()),
            (2, 2, 1, 5)
        );
    }
}

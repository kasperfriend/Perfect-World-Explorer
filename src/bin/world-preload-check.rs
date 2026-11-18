#![allow(dead_code)]
#[path = "../bmd.rs"]
mod bmd;
// The baked draw-state table is shared with the production scenery path.
#[path = "../bmd_baked.rs"]
mod bmd_baked;
#[path = "../objects.rs"]
mod objects;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../scenery_visibility.rs"]
mod scenery_visibility;
#[path = "../texture_atlas.rs"]
mod texture_atlas;
#[path = "../texture_resolution.rs"]
mod texture_resolution;
#[path = "../world_objects.rs"]
mod world_objects;
#[path = "../world_page_cache.rs"]
mod world_page_cache;
mod gpu {
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    pub(crate) struct Vertex {
        pub(crate) pos: [f32; 3],
        pub(crate) normal: [f32; 3],
        pub(crate) uv: [f32; 2],
        pub(crate) color: [f32; 4],
        pub(crate) kind: f32,
        pub(crate) diffuse: u32,
        pub(crate) night_diffuse: u32,
    }
}
fn main() {
    use std::sync::{atomic::AtomicBool, mpsc, Arc};
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("element folder"));
    let dir = root.join("maps/world");
    let world = scene::Scene::load(&dir.join("world.ecwld")).unwrap();
    let ornaments = world.load_ornaments(&dir.join("world.ecbsd")).unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    let count = ornaments.len();
    std::thread::spawn(move || {
        world_objects::build(root, ornaments, tx, Arc::new(AtomicBool::new(false)))
    });
    let (mut pages, mut verts, mut tris) = (0, 0, 0);
    let mut atlas_bytes = 0usize;
    for message in rx {
        match message {
            world_objects::Message::Page(p) => {
                pages += 1;
                verts += p.vertices.len();
                atlas_bytes += p.atlas.len();
                tris += p.indices.len() / 3;
                println!(
                    "Page {pages}: {} placements, {verts} vertices; shared atlas {:?}",
                    p.processed, p.shared_atlas
                )
            }
            world_objects::Message::CacheInvalid => {
                println!("discarding invalid cached pages and rebuilding");
            }
            world_objects::Message::Done {
                processed,
                skipped,
                stats,
                world_draw_cmds: _,
            } => {
                println!(
                    "residency: decodes={} alias_hits={} alias_bytes_saved={} evictions={} evicted_bytes={} peak_resident_bytes={} resident_bytes={} pinned_over_budget={} budget_bytes={}",
                    stats.residency.decodes, stats.residency.alias_hits, stats.residency.alias_bytes_saved,
                    stats.residency.evictions, stats.residency.evicted_bytes, stats.residency.peak_resident_bytes,
                    stats.residency.resident_bytes, stats.residency.pinned_over_budget, stats.residency.budget_bytes
                );
                println!(
                    "resolver: queries={} cache_hits={} trivial={} shader_registry={} archive_hits={} disk_hits={} missing={} missing_quiet={} nrf_derived={} nrf_derived_present={} nrf_default_fallback={} shader_registry_file_backed={} unresolved_rows={} pages={}",
                    stats.resolver.queries, stats.resolver.cache_hits, stats.resolver.trivial, stats.resolver.shader_registry,
                    stats.resolver.archive_hits, stats.resolver.disk_hits, stats.resolver.missing, stats.resolver.missing_quiet,
                    stats.resolver.nrf_derived, stats.resolver.nrf_derived_present, stats.resolver.nrf_default_fallback,
                    stats.resolver.shader_registry_file_backed, stats.unresolved_rows, stats.pages
                );
                println!(
                    "Unique GPU atlases: {}, exact bytes including mips: {}",
                    stats.atlas_pages, stats.atlas_bytes
                );
                println!(
                    "unavailable scenery models: {}{}",
                    stats.unavailable_models.len(),
                    if stats.unavailable_examples.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", stats.unavailable_examples.join("; "))
                    }
                );
                println!("Done {processed}/{count}, {pages} pages, {verts} vertices, {tris} triangles, {skipped} missing-texture meshes; atlas base {:.1} MiB, with mips ~{:.1} MiB",atlas_bytes as f64/1048576.,atlas_bytes as f64/1048576.*(1.-1./65536.)/0.75);
                return;
            }
            world_objects::Message::Error(e) => panic!("{e}"),
        }
    }
    panic!("worker ended prematurely")
}

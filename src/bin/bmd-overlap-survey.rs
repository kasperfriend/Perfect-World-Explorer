//! Diagnose exactly coincident BMD triangles; does not change rendering.
#![allow(dead_code)]
#[path = "../bmd.rs"]
mod bmd;
#[path = "../bmd_baked.rs"]
mod bmd_baked;
#[path = "../pck.rs"]
mod pck;
use std::{collections::HashMap, env, path::Path};
fn main() {
    let path = env::args()
        .nth(1)
        .expect("usage: bmd-overlap-survey litmodels.pck");
    let mut f = pck::PackageFile::open(Path::new(&path)).unwrap();
    let entries = pck::directory(&mut f).unwrap();
    let mut models = 0;
    let mut unparsed = 0u64;
    let mut meshes = 0u64;
    let mut duplicate_models = 0;
    let mut total = 0u64;
    let mut same = 0u64;
    let mut reverse = 0u64;
    let mut cross = 0u64;
    let mut examples = 0;
    let mut culled = 0u64;
    let mut later = 0u64;
    let mut cross_mesh = 0u64;
    let mut positive = 0u64;
    let mut negative = 0u64;
    for e in entries.iter().filter(|e| {
        e.name.to_lowercase().starts_with("litmodels/world/")
            && e.name.to_lowercase().ends_with(".bmd")
    }) {
        let bytes = pck::read_entry(&mut f, e).unwrap();
        // A single unreadable model must not hide the census of the rest.
        let Ok(m) = bmd::Model::parse(&bytes) else {
            unparsed += 1;
            continue;
        };
        models += 1;
        meshes += m.meshes.len() as u64;
        let mut seen: HashMap<[[u32; 3]; 3], (usize, String, bool)> = HashMap::new();
        let mut duplicate = 0;
        let two_sided: Vec<bool> = m
            .meshes
            .iter()
            .map(|mesh| mesh.material.as_ref().is_some_and(|mat| mat.two_sided))
            .collect();
        for (mi, mesh) in m.meshes.iter().enumerate() {
            for triangle in mesh.indices.chunks_exact(3) {
                let a = &mesh.vertices[triangle[0] as usize];
                let b = &mesh.vertices[triangle[1] as usize];
                let c = &mesh.vertices[triangle[2] as usize];
                let u = [
                    b.position[0] - a.position[0],
                    b.position[1] - a.position[1],
                    b.position[2] - a.position[2],
                ];
                let v = [
                    c.position[0] - a.position[0],
                    c.position[1] - a.position[1],
                    c.position[2] - a.position[2],
                ];
                let n = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                let dot = n[0] * a.normal[0] + n[1] * a.normal[1] + n[2] * a.normal[2];
                positive += (dot > 0.00001) as u64;
                negative += (dot < -0.00001) as u64;
                let mut ps = [[0u32; 3]; 3];
                for i in 0..3 {
                    for j in 0..3 {
                        let x = mesh.vertices[triangle[i] as usize].position[j];
                        ps[i][j] = if x == 0. { 0 } else { x.to_bits() };
                    }
                }
                let parity = (ps[0] > ps[1]) as u8 + (ps[0] > ps[2]) as u8 + (ps[1] > ps[2]) as u8;
                ps.sort_unstable();
                if let Some((other, tex, old_parity)) = seen.get(&ps) {
                    duplicate += 1;
                    cross += (*other != mi) as u64;
                    let same_winding = *old_parity == (parity % 2 == 1);
                    if same_winding {
                        same += 1
                    } else {
                        reverse += 1
                    };
                    // A model is submitted mesh after mesh and the viewer keeps
                    // that order even when a page split turns one model into
                    // several draws, so the later triangle of a pair is the
                    // later primitive on the device as well.
                    let outcome = bmd_baked::overlap_outcome(
                        same_winding,
                        two_sided[*other],
                        two_sided[mi],
                        true,
                    );
                    match outcome {
                        bmd_baked::OverlapOutcome::Culled => culled += 1,
                        bmd_baked::OverlapOutcome::LaterSubmissionWins => {
                            later += 1;
                            cross_mesh += (*other != mi) as u64;
                        }
                        bmd_baked::OverlapOutcome::Unresolved => {}
                    }
                    if examples < 12 {
                        println!(
                            "{} duplicate mesh {other} ({tex}) / {mi} ({}) {outcome:?}",
                            e.name, mesh.texture
                        );
                        examples += 1;
                    }
                } else {
                    seen.insert(ps, (mi, mesh.texture.clone(), parity % 2 == 1));
                }
            }
        }
        if duplicate > 0 {
            duplicate_models += 1;
            total += duplicate;
        }
    }
    println!("models={models} meshes={meshes} with_exact_duplicate_triangles={duplicate_models} duplicate_triangles={total} same_winding={same} opposite_winding={reverse} across_meshes={cross} normals_positive={positive} normals_negative={negative} resolved_by_cull={culled} resolved_by_order={later} resolved_cross_mesh={cross_mesh} unparsed={unparsed}");
}

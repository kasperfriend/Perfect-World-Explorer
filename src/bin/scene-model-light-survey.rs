//! Survey actual EC model placements against static scene-light grid.
//! This tests spatial selection only: model eligibility and effect lights
//! are unknown; no models are drawn or relit by this diagnostic.
#![allow(dead_code)]
#[path = "../animation.rs"]
mod animation;
#[path = "../ecmodel.rs"]
mod ecmodel;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../scene_lights.rs"]
mod scene_lights;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let scene = PathBuf::from(
        args.next()
            .ok_or("usage: scene-model-light-survey world.ecwld scenelights.dat")?,
    );
    let lights = PathBuf::from(args.next().ok_or("missing scenelights.dat")?);
    let world = scene::Scene::load(&scene)?;
    let lights = scene_lights::SceneLights::load(&lights)?;
    let mut models = BTreeMap::new();
    for m in world.blocks.iter().flat_map(|b| &b.models) {
        models.entry(m.export_id).or_insert(m);
    }
    let mut day_count = 0;
    let mut night_count = 0;
    let mut nearest = BTreeSet::new();
    let mut flagged = 0;
    let mut diff = 0;
    let mut no_light = 0;
    for (id, m) in &models {
        let mut grid = scene_lights::StaticGrid::default();
        grid.update(&lights, m.position);
        let day = grid.query(m.position, 0.);
        let night = grid.query(m.position, 1.);
        assert_eq!(day.is_some(), night.is_some());
        if let (Some(d), Some(n)) = (day, night) {
            day_count += 1;
            night_count += 1;
            assert_eq!(d.position, n.position);
            nearest.insert((
                d.position[0].to_bits(),
                d.position[1].to_bits(),
                d.position[2].to_bits(),
            ));
            flagged += usize::from(d.flags & 1 != 0);
            diff += usize::from(d.diffuse != n.diffuse || d.ambient != n.ambient);
            let distance = ((d.position[0] - m.position[0]).powi(2)
                + (d.position[1] - m.position[1]).powi(2)
                + (d.position[2] - m.position[2]).powi(2))
            .sqrt();
            println!(
                "id {id} pos {:?} light {:?} distance {:.2} range {:.2} flag {:#x}",
                m.position, d.position, distance, d.range, d.flags
            );
        } else {
            no_light += 1;
        }
    }
    println!("{} unique placements, {day_count} day and {night_count} night candidates, {no_light} none, {flagged} flagged, {diff} changed day/night, {} distinct selected positions",models.len(),nearest.len());
    Ok(())
}

//! Record-level tree/effect census for one ECWLD and its paired EC-BSD.
//!
//! This uses the production `scene::Scene` parser. It counts authored tree
//! types/placements and effect root paths, then reports invariants useful for
//! checking a field-layout change against real map bytes. It does not open the
//! tree/GFX archives or claim that their referenced assets render.

#[allow(dead_code)]
#[path = "../scene.rs"]
mod scene;

// `scene.rs`'s own tests are compiled into this binary under `cargo test
// --bins`; their only `pck` dependency is the explicit corpus-skip reporter.
#[cfg(test)]
pub mod pck {
    pub fn corpus_absent(what: &str) {
        eprintln!("SKIPPED: {what} is not staged for scene-census tests");
    }
}

use std::{
    collections::{BTreeMap, HashSet},
    env, fs,
    path::PathBuf,
};

fn field(s: &str) -> String {
    s.replace('\\', "/")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn vec_len(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn run() -> Result<(), String> {
    let mut args = env::args_os().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "usage: scene-tree-effect-census <world.ecwld> [world.ecbsd]".to_string())?;
    let ecbsd = args.next().map(PathBuf::from).or_else(|| {
        let paired = path.with_extension("ecbsd");
        paired.is_file().then_some(paired)
    });
    if args.next().is_some() {
        return Err("usage: scene-tree-effect-census <world.ecwld> [world.ecbsd]".into());
    }

    let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let version = u32::from_le_bytes(
        bytes
            .get(4..8)
            .ok_or("world file is shorter than its header")?
            .try_into()
            .unwrap(),
    );
    let world = scene::Scene::parse(&bytes)?;
    let mut tree_uses = vec![0usize; world.tree_types.len()];
    let mut tree_min = [f32::INFINITY; 3];
    let mut tree_max = [f32::NEG_INFINITY; 3];
    let mut effects = 0usize;
    let mut modes = [0usize; 4];
    let mut ids = HashSet::new();
    let mut roots = BTreeMap::<String, usize>::new();
    let mut alphas = BTreeMap::<u32, usize>::new();
    let (mut min_scale, mut max_scale) = (f32::INFINITY, f32::NEG_INFINITY);
    let (mut min_speed, mut max_speed) = (f32::INFINITY, f32::NEG_INFINITY);
    let (mut bad_scale, mut bad_speed, mut bad_basis) = (0usize, 0usize, 0usize);
    let (mut max_dir_error, mut max_up_error, mut max_dot) = (0.0f32, 0.0f32, 0.0f32);

    let mut counts = [0usize; 10];
    for block in &world.blocks {
        counts[0] += block.trees.len();
        counts[1] += block.water.len();
        counts[2] += block.grass.len();
        counts[3] += block.ornaments.len();
        counts[4] += block.boxes.len();
        counts[5] += block.effects.len();
        counts[6] += block.models.len();
        counts[7] += block.critters.len();
        counts[8] += block.beziers.len();
        counts[9] += block.sounds.len();

        for tree in &block.trees {
            let Some(uses) = tree_uses.get_mut(tree.tree_type as usize) else {
                return Err(format!(
                    "block ({},{}): tree type {} exceeds the {}-entry table",
                    block.row,
                    block.col,
                    tree.tree_type,
                    tree_uses.len()
                ));
            };
            *uses += 1;
            for axis in 0..3 {
                tree_min[axis] = tree_min[axis].min(tree.position[axis]);
                tree_max[axis] = tree_max[axis].max(tree.position[axis]);
            }
        }

        for effect in &block.effects {
            effects += 1;
            ids.insert(effect.export_id);
            *roots.entry(effect.path.clone()).or_default() += 1;
            *alphas.entry(effect.alpha.to_bits()).or_default() += 1;
            match effect.valid_time {
                0 => modes[0] += 1,
                1 => modes[1] += 1,
                2 => modes[2] += 1,
                _ => modes[3] += 1,
            }
            min_scale = min_scale.min(effect.scale);
            max_scale = max_scale.max(effect.scale);
            min_speed = min_speed.min(effect.speed);
            max_speed = max_speed.max(effect.speed);
            if !effect.scale.is_finite() || effect.scale <= 0.0 {
                bad_scale += 1;
            }
            if !effect.speed.is_finite() || effect.speed <= 0.0 {
                bad_speed += 1;
            }
            let dir_len = vec_len(effect.direction);
            let up_len = vec_len(effect.up);
            let dot = (effect.direction[0] * effect.up[0]
                + effect.direction[1] * effect.up[1]
                + effect.direction[2] * effect.up[2])
                / (dir_len * up_len).max(f32::MIN_POSITIVE);
            let dir_error = (dir_len - 1.0).abs();
            let up_error = (up_len - 1.0).abs();
            max_dir_error = max_dir_error.max(dir_error);
            max_up_error = max_up_error.max(up_error);
            max_dot = max_dot.max(dot.abs());
            if dir_error > 0.01 || up_error > 0.01 || dot.abs() > 0.01 {
                bad_basis += 1;
            }
        }
    }

    println!(
        "input\t{}\t{} bytes\tECWLD v{}",
        field(&path.display().to_string()),
        bytes.len(),
        version
    );
    println!(
        "header\tgrid={}x{}\tblock_size={:.0}\twidth={:.3}\tlength={:.3}",
        world.rows, world.cols, world.block_size, world.width, world.length
    );
    println!(
        "scene_counts\tblocks={}\ttree_types={}\tgrass_types={}\ttrees={}\twater={}\tgrass={}\tornament_refs={}\tboxes={}\teffects={}\tmodels={}\tcritters={}\tbeziers={}\tsounds={}",
        world.blocks.len(),
        world.tree_types.len(),
        world.grass_types.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        counts[4],
        counts[5],
        counts[6],
        counts[7],
        counts[8],
        counts[9]
    );
    println!("tree_type\tindex\tplacements\tspt\tcomposite_dds");
    for (index, (tree, uses)) in world.tree_types.iter().zip(&tree_uses).enumerate() {
        println!(
            "tree_type\t{index}\t{uses}\t{}\t{}",
            field(&tree.tree),
            field(&tree.composite)
        );
    }
    if counts[0] > 0 {
        println!(
            "tree_positions\tx={}..{}\ty={}..{}\tz={}..{}",
            tree_min[0], tree_max[0], tree_min[1], tree_max[1], tree_min[2], tree_max[2]
        );
    }

    println!(
        "effect_records\ttotal={effects}\tunique_ids={}\tunique_roots={}\tday_only={}\tnight_only={}\tboth={}\tother_mode={}\tscale={}..{}\tspeed={}..{}\tbad_scale={bad_scale}\tbad_speed={bad_speed}\tbad_basis={bad_basis}\tmax_dir_len_error={max_dir_error}\tmax_up_len_error={max_up_error}\tmax_abs_dir_up_dot={max_dot}",
        ids.len(),
        roots.len(),
        modes[0],
        modes[1],
        modes[2],
        modes[3],
        min_scale,
        max_scale,
        min_speed,
        max_speed
    );
    let mut alpha_rows: Vec<_> = alphas
        .iter()
        .map(|(&bits, &count)| (f32::from_bits(bits), count))
        .collect();
    alpha_rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.total_cmp(&b.0)));
    println!("effect_alpha\tvalue\tcount");
    for (alpha, count) in alpha_rows {
        println!("effect_alpha\t{alpha}\t{count}");
    }
    println!("effect_root\tplacements\tpath");
    for (root, count) in roots {
        println!("effect_root\t{count}\t{}", field(&root));
    }

    if let Some(ecbsd) = ecbsd {
        let records = world
            .load_ornaments(&ecbsd)
            .map_err(|e| format!("{}: {e}", ecbsd.display()))?;
        let header = scene::ecbsd_header(
            &fs::read(&ecbsd).map_err(|e| format!("{}: {e}", ecbsd.display()))?,
        )?;
        println!(
            "ecbsd\t{}\trevision={}\tunique_ornament_records={}",
            field(&ecbsd.display().to_string()),
            header,
            records.len()
        );
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

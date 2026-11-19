//! Feed decoded little-endian f32 heights on stdin, in source raster order.
//! Export the production planner's source-grid indices (not a render).
#[path = "../modules/lighter/terrain_fidelity.rs"]
mod terrain_fidelity;

use std::io::{self, Read, Write};
use terrain_fidelity::Layout;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dims: Vec<usize> = std::env::args()
        .skip(1)
        .map(|s| s.parse())
        .collect::<Result<_, _>>()?;
    let [width, height] = dims[..] else {
        return Err("usage: lighter-terrain-plan WIDTH HEIGHT < heights.f32le".into());
    };
    let count = width
        .checked_mul(height)
        .filter(|&n| width >= 2 && height >= 2 && n <= 100_000_000)
        .ok_or("unsupported heightfield extent")?;
    let mut raw = Vec::new();
    io::stdin()
        .take((4 * count + 1) as u64)
        .read_to_end(&mut raw)?;
    if raw.len() != count * 4 {
        return Err("heightfield byte length does not match dimensions".into());
    }
    let layout = terrain_fidelity::choose(width, height, |x, y| {
        let at = (y * width + x) * 4;
        let z = f32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
        if z.is_finite() {
            Ok(z)
        } else {
            Err("non-finite terrain input".into())
        }
    })?;
    let mut out = io::BufWriter::new(io::stdout().lock());
    match layout {
        Layout::Regular(plan) => {
            writeln!(out, "regular {} {:?}", plan.step, plan.max_sample_error)?
        }
        Layout::Refined(plan) => {
            writeln!(
                out,
                "adaptive {} {} {} {}",
                terrain_fidelity::ADAPTIVE_STEP,
                plan.index_count,
                plan.refined,
                plan.max_error
            )?;
            let [cols, rows] = plan.tiles();
            for y in 0..rows {
                for x in 0..cols {
                    let mut status = Ok(());
                    plan.triangles(x, y, |[[ax, ay], [bx, by], [cx, cy]]| {
                        if status.is_ok() {
                            status = writeln!(out, "{ax} {ay} {bx} {by} {cx} {cy}");
                        }
                    });
                    status?;
                }
            }
        }
    }
    out.flush()?;
    Ok(())
}

//! Standalone reader check; compile with rustc --edition=2021.
#[path = "../../src/tree_component_cache.rs"]
mod tree_component_cache;
use std::{env, fs};
fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: check-component-cache <pwc> <spt> <dll> <type>");
        std::process::exit(2);
    }
    let result = (|| -> Result<(), String> {
        let data = fs::read(&args[1]).map_err(|e| e.to_string())?;
        let source = fs::read(&args[2]).map_err(|e| e.to_string())?;
        let dll = fs::read(&args[3]).map_err(|e| e.to_string())?;
        let id: u32 = args[4].parse().map_err(|_| "invalid tree type")?;
        let c = tree_component_cache::parse(&data, &source, &dll, id)?;
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            c.branches.len(),
            c.fronds.len(),
            c.leaves.len(),
            c.branches.iter().map(|p| p.indices.len()).sum::<usize>(),
            c.fronds.iter().map(|p| p.indices.len()).sum::<usize>(),
            c.leaves.iter().map(Vec::len).sum::<usize>()
        );
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

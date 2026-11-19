//! What a `precinct.clt` actually contains, per file.
//!
//! The register and the code comments both state "the 1.8.4 precinct is 383
//! zones, 783 tracks and 347 stereo ambience paths". The file staged beside the
//! maps in this checkout is 29 zones with no ambience at all. Both statements
//! are true of different files -- the 383-zone one is 885,012 B and is the
//! world map's -- and nothing in the tree could tell them apart, so a claim
//! could not be checked against anything.
//!
//! This prints the facts a claim has to match: zones, playlist tracks, zones
//! with a day and a night path, distinct paths, malformed blocks, and the
//! per-extension census of every path the file references. It exits non-zero if
//! an extension the file reaches is outside `music::DECODABLE_EXTENSIONS`,
//! which is the check that matters for a future release.
#![allow(dead_code)]
#[path = "../music.rs"]
mod music;
#[path = "../pck.rs"]
mod pck;

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("precinct.clt"))
        {
            out.push(p);
        }
    }
}

fn main() {
    let roots: Vec<String> = std::env::args().skip(1).collect();
    if roots.is_empty() {
        eprintln!("usage: precinct-check <root>...   (searches each root for precinct.clt)");
        std::process::exit(2);
    }
    let mut files = Vec::new();
    for root in &roots {
        walk(std::path::Path::new(root), &mut files);
    }
    if files.is_empty() {
        eprintln!("no precinct.clt under the given roots");
        std::process::exit(2);
    }
    files.sort();
    println!(
        "{:<52} {:>4} {:>6} {:>7} {:>7} {:>7} {:>6} {:>6}",
        "file", "zones", "tracks", "day", "night", "paths", "bad", "ext"
    );
    let mut failed = 0;
    for f in &files {
        match music::Zones::open(f) {
            Ok(z) => {
                let tracks: usize = z.zones.iter().map(|x| x.tracks.len()).sum();
                let day = z
                    .zones
                    .iter()
                    .filter(|x| !x.ambience_day.is_empty())
                    .count();
                let night = z
                    .zones
                    .iter()
                    .filter(|x| !x.ambience_night.is_empty())
                    .count();
                let mut paths: Vec<&str> = z
                    .zones
                    .iter()
                    .flat_map(|x| [x.ambience_day.as_str(), x.ambience_night.as_str()])
                    .filter(|p| !p.is_empty())
                    .collect();
                paths.sort_unstable();
                paths.dedup();
                let mut ext: std::collections::BTreeMap<&str, usize> =
                    std::collections::BTreeMap::new();
                for p in z
                    .zones
                    .iter()
                    .flat_map(|x| x.tracks.iter())
                    .chain(
                        z.zones
                            .iter()
                            .flat_map(|x| [&x.ambience_day, &x.ambience_night]),
                    )
                    .filter(|p| !p.is_empty())
                {
                    let e = p.rsplit_once('.').map(|(_, e)| e).unwrap_or("?");
                    *ext.entry(e).or_default() += 1;
                }
                println!(
                    "{:<52} {:>4} {:>6} {:>7} {:>7} {:>7} {:>6} {:>6}",
                    f.display().to_string(),
                    z.zones.len(),
                    tracks,
                    day,
                    night,
                    paths.len(),
                    z.malformed,
                    ext.len()
                );
                let census: Vec<String> = ext.iter().map(|(e, n)| format!("{n} {e}")).collect();
                println!("{:>60} {}", "", census.join(", "));
                // A decoder list is only correct if it covers what the file
                // names; this is the assertion the row in the register makes.
                let unknown: Vec<&str> = ext
                    .keys()
                    .copied()
                    .filter(|e| {
                        !music::DECODABLE_EXTENSIONS
                            .iter()
                            .any(|d| d.eq_ignore_ascii_case(e))
                    })
                    .collect();
                if !unknown.is_empty() {
                    eprintln!(
                        "{}: no decoder for {} (this build decodes {})",
                        f.display(),
                        unknown.join(", "),
                        music::DECODABLE_EXTENSIONS.join(", ")
                    );
                    failed += 1;
                }
            }
            Err(e) => {
                eprintln!("{}: FAILED: {e}", f.display());
                failed += 1;
            }
        }
    }
    std::process::exit(if failed == 0 { 0 } else { 1 });
}

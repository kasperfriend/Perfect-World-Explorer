#[path = "../pck.rs"]
mod pck;
use std::{
    env,
    io::{self, Write},
    path::Path,
};
fn run() -> Result<(), String> {
    let a: Vec<_> = env::args().collect();
    if a.len() != 2 && a.len() != 3 {
        return Err("usage: pck-inspect archive.pck [entry-name > output]".into());
    }
    let mut f = pck::PackageFile::open(Path::new(&a[1])).map_err(|e| e.to_string())?;
    let entries = pck::directory(&mut f)?;
    if a.len() == 2 {
        for e in entries {
            println!("{}\t{}\t{}", e.size, e.compressed, e.name)
        }
    } else {
        let e = entries
            .iter()
            .find(|x| x.name.eq_ignore_ascii_case(&a[2].replace('\\', "/")))
            .ok_or("entry not found")?;
        let out = pck::read_entry(&mut f, e)?;
        io::stdout().write_all(&out).map_err(|e| e.to_string())?
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1)
    }
}

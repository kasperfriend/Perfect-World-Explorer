#[path = "../angelica_pck.rs"]
mod angelica_pck;
#[path = "../pck.rs"]
mod pck;
use std::{
    env,
    fs::File,
    io::{self, Write},
};
fn run() -> Result<(), String> {
    let a: Vec<_> = env::args().collect();
    if a.len() != 2 && a.len() != 3 {
        return Err("usage: pck-inspect archive.pck [entry-name > output]".into());
    }
    let mut f = File::open(&a[1]).map_err(|e| e.to_string())?;
    // The two Angelica containers share their prefix and are told apart by the
    // guard pair, exactly as `client_detect::scan_archive` decides it. Trying
    // only the retail reader here meant `configs.pck` and `shaders.pck` could
    // not be listed by the tool whose job is listing packages.
    let entries = match pck::directory(&mut f) {
        Ok(entries) => entries,
        Err(pck_error) => angelica_pck::directory(&mut f)
            .map_err(|angelica_error| format!("{pck_error}; {angelica_error}"))?,
    };
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

//! Tree source diagnostic; the Windows viewer prepares trees automatically.
#[allow(dead_code)]
#[path = "../pck.rs"]
mod pck;
#[allow(dead_code)]
#[path = "../scene.rs"]
mod scene;
// `tree_source.rs` counts the placements it loses, through `crate::load_status`.
#[path = "../load_status.rs"]
mod load_status;
#[path = "../tree_source.rs"]
mod tree_source;
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        eprintln!("usage: tree-source <client-root> <new-output-directory>");
        std::process::exit(2)
    }
    if let Err(e) = tree_source::extract(
        std::path::Path::new(&args[1]),
        std::path::Path::new(&args[2]),
    ) {
        eprintln!("{e}");
        std::process::exit(1)
    }
}

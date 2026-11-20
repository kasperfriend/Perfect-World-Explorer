//! Headless check of the client's own loading-screen artwork.
//!
//! The Windows loading window fills the screen with a randomly chosen picture
//! from the selected client's `surfaces.pck`, then draws the DirectStorage
//! state, the read-throughput graph, the read totals, the progress message and
//! an indeterminate loading bar over it. This binary exercises everything
//! except GDI itself: the archive lookup, the random choice, the JPEG/BMP/TGA/
//! DDS decode, the cover fit to a requested window size, the darkened status
//! strip, and the BGRA conversion the `StretchDIBits` call consumes.
//!
//! Usage:
//!
//! ```text
//! loading-screen-check <client-root-or-element-folder> [output.png] [width] [height]
//! ```
//!
//! It prints every candidate picture, the one the random draw selected (or the
//! `PWE_LOADING_SCREEN` override), the decoded source size and the fitted size,
//! then writes a PNG whose bottom strip is blended exactly as the window blends
//! it. Exit code 0 means the picture path works for that client; a nonzero code
//! names the failure, so this doubles as the verification step for a staged
//! retail client.
use std::{path::Path, process::ExitCode};

// The production module declares the full artwork API; a check binary uses
// part of it, and an unused helper here is not a defect in the viewer.
#[allow(dead_code)]
#[path = "../loading_screen.rs"]
mod loading_screen;
#[allow(dead_code)]
#[path = "../pck.rs"]
mod pck;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!(
            "usage: loading-screen-check <client-root-or-element-folder> [output.png] [width] [height]"
        );
        return ExitCode::from(2);
    };
    let root = Path::new(&root);
    let output = args.next();
    // `parse` is fallible and the argument itself is optional, so each size is
    // `Result<u32, String>`: absent means the default, malformed means exit 2.
    let width = match args.next() {
        Some(value) => match value.parse::<u32>() {
            Ok(width) => Ok(width),
            Err(error) => Err(format!("invalid width: {error}")),
        },
        None => Ok(1920),
    };
    let height = match args.next() {
        Some(value) => match value.parse::<u32>() {
            Ok(height) => Ok(height),
            Err(error) => Err(format!("invalid height: {error}")),
        },
        None => Ok(1080),
    };
    let (width, height) = match (width, height) {
        (Ok(width), Ok(height)) if width >= 64 && height >= 64 => (width, height),
        (Ok(width), Ok(height)) => {
            eprintln!("window size {width}x{height} is too small; use at least 64x64");
            return ExitCode::from(2);
        }
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    match run(root, output.as_deref(), width, height) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("loading-screen-check: {error}");
            ExitCode::from(1)
        }
    }
}

fn run(root: &Path, output: Option<&str>, width: u32, height: u32) -> Result<bool, String> {
    let element = loading_screen::element_folder(root);
    let archive = element.join(loading_screen::ARCHIVE_NAME);
    println!("client argument: {}", root.display());
    println!("element folder:  {}", element.display());
    println!("archive:         {}", archive.display());
    let pictures = loading_screen::list(&element)?;
    println!("{} root-level picture(s) in the archive:", pictures.len());
    for picture in &pictures {
        println!("  {} ({} bytes)", picture.path, picture.size);
    }
    if let Some(name) = loading_screen::forced_picture_name() {
        println!("PWE_LOADING_SCREEN override: {name}");
    }
    let random_word = loading_screen::random_word();
    let picture = loading_screen::choose(&element, random_word)?;
    println!(
        "random draw (word {random_word:#x}) selected: {}",
        picture.path
    );
    let image = loading_screen::decode(&element, &picture)?;
    let (source_width, source_height) = image.dimensions();
    println!("decoded {source_width}x{source_height} RGBA");
    let fitted = loading_screen::fill(&image, width, height)?;
    let blended = loading_screen::overlay_strip(fitted.as_raw(), width, height, 178, 72, 0.78)?;
    let bgra = loading_screen::to_bgra(&blended);
    if bgra.len() != (width as usize) * (height as usize) * 4 {
        return Err(format!(
            "BGRA surface is {} bytes, expected {}x{}x4",
            bgra.len(),
            width,
            height
        ));
    }
    // The overlay strip must actually darken the bottom rows: a picture path
    // that silently skipped the blend would make the status text unreadable on
    // a bright loading screen.
    let row = (height as usize - 4) * width as usize * 4;
    let fitted_row = &fitted.as_raw()[row..row + 4];
    let blended_row = &blended[row..row + 4];
    // Sum in `u32`: three `u8` channels at 255 overflow `u8` and would panic in
    // a debug build on exactly the bright artwork this check exists for.
    let brightness = |channels: &[u8]| -> u32 { channels[..3].iter().map(|c| *c as u32).sum() };
    if brightness(blended_row) >= brightness(fitted_row) {
        return Err("the status strip did not darken the bottom rows".into());
    }
    println!(
        "fitted {width}x{height}; bottom row RGB {:?} -> {:?} under the status strip",
        &fitted_row[..3],
        &blended_row[..3]
    );
    println!(
        "BGRA surface: {} bytes, top-down, ready for StretchDIBits",
        bgra.len()
    );
    if let Some(output) = output {
        let path = Path::new(output);
        let png = image::RgbaImage::from_raw(width, height, blended)
            .ok_or_else(|| "fitted RGBA buffer does not match its dimensions".to_string())?;
        loading_screen::write_fixture(&png, path)?;
        println!("wrote {}", path.display());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn loading_screen_module_is_shared_with_the_viewer() {
        // The binary declares the production module by path, so a change to the
        // viewer's lookup, decode or fit rules is what this check exercises.
        super::loading_screen::list(std::path::Path::new(".")).unwrap_err();
    }
}

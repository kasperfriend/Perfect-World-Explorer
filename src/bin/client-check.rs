//! Print everything the folder scan found: the executable identity, every loose
//! data-file version and every archive's container version.
//!
//! This is the non-GUI form of what the launcher shows after **Select Game
//! Folder**, so a report can be pasted from a terminal or a CI run.
//!
//! ```text
//! cargo run --bin client-check -- "C:\Games\Perfect World\element"
//! ```
//!
//! Exit status: `0` when the folder is loadable (unknown versions included —
//! they are reported, not refused), `1` when the world or a required archive is
//! missing. `--json` prints one machine-readable object per file instead of the
//! text report.
#![allow(dead_code)] // the file modules carry their full production surface
#[path = "../angelica_pck.rs"]
mod angelica_pck;
#[path = "../client_detect.rs"]
mod client_detect;
#[path = "../pck.rs"]
mod pck;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let mut json = false;
    let mut path = None;
    for arg in args.by_ref() {
        let text = arg.to_string_lossy();
        if text == "--json" {
            json = true;
        } else if text == "--help" || text == "-h" {
            println!("usage: client-check <client root or element folder> [--json]");
            return;
        } else if path.is_none() {
            path = Some(arg);
        }
    }
    let Some(path) = path else {
        eprintln!("usage: client-check <client root or element folder> [--json]");
        std::process::exit(2);
    };
    let scan = client_detect::scan(std::path::Path::new(&path));
    if json {
        print_json(&scan);
    } else {
        print!("{}", scan.report());
        println!();
        println!("Result: {}", scan.headline());
    }
    std::process::exit(if scan.loadable() { 0 } else { 1 });
}

/// A flat JSON object: enough for a CI job to assert on a version without a
/// scraping a human report. Hand-written because the crate has no serializer.
fn print_json(scan: &client_detect::Scan) {
    fn quote(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
    let mut fields: Vec<String> = Vec::new();
    fields.push(format!(
        "  \"element\": {}",
        quote(&scan.element.to_string_lossy())
    ));
    fields.push(format!("  \"loadable\": {}", scan.loadable()));
    if let Some(exe) = &scan.executable {
        fields.push(format!("  \"executable\": {}", quote(&exe.label())));
        fields.push(format!("  \"executable_known\": {}", exe.is_known()));
        fields.push(format!("  \"executable_size\": {}", exe.size));
        fields.push(format!("  \"executable_sha256\": {}", quote(&exe.sha256)));
        if let Some(version) = &exe.file_version {
            fields.push(format!("  \"executable_file_version\": {}", quote(version)));
        }
        if let Some(version) = &exe.product_version {
            fields.push(format!(
                "  \"executable_product_version\": {}",
                quote(version)
            ));
        }
    }
    // The data version is the `(elements, tasks)` pair, reported apart from the
    // executable because one build can ship several revisions.
    let (elements_version, tasks_version) = scan.data_versions();
    fields.push(format!(
        "  \"elements_version\": {}",
        elements_version
            .as_deref()
            .map(quote)
            .unwrap_or_else(|| "null".into())
    ));
    fields.push(format!(
        "  \"tasks_version\": {}",
        tasks_version
            .as_deref()
            .map(quote)
            .unwrap_or_else(|| "null".into())
    ));
    fields.push(format!(
        "  \"data_version\": {}",
        quote(&scan.data_version_summary())
    ));
    if let Some(elements) = &scan.elements {
        // The undecoded header word stays available for tooling that wants the
        // exact bytes rather than the label.
        fields.push(format!(
            "  \"elements_raw\": {}",
            elements
                .raw
                .map(|raw| format!("{raw}"))
                .unwrap_or_else(|| "null".into())
        ));
        fields.push(format!(
            "  \"elements_label\": {}",
            quote(elements.version.as_deref().unwrap_or(""))
        ));
    }
    let data: Vec<String> = scan
        .data_files
        .iter()
        .map(|info| {
            format!(
                "    {{\"name\": {}, \"version\": {}, \"raw\": {}}}",
                quote(&info.name),
                quote(info.version.as_deref().unwrap_or("")),
                info.raw
                    .map(|raw| raw.to_string())
                    .unwrap_or_else(|| "null".into())
            )
        })
        .collect();
    fields.push(format!("  \"data_files\": [\n{}\n  ]", data.join(",\n")));
    let archives: Vec<String> = scan
        .archives
        .iter()
        .map(|archive| {
            format!(
                concat!(
                    "    {{\"name\": {}, \"container\": {}, \"version\": {}, ",
                    "\"entries\": {}, \"problem\": {}}}"
                ),
                quote(&archive.name),
                quote(archive.kind.label()),
                archive
                    .version
                    .map(|version| version.to_string())
                    .unwrap_or_else(|| "null".into()),
                archive
                    .entries
                    .map(|entries| entries.to_string())
                    .unwrap_or_else(|| "null".into()),
                archive
                    .problem
                    .as_deref()
                    .map(quote)
                    .unwrap_or_else(|| "null".into())
            )
        })
        .collect();
    fields.push(format!("  \"archives\": [\n{}\n  ]", archives.join(",\n")));
    let errors: Vec<String> = scan.errors.iter().map(|error| quote(error)).collect();
    fields.push(format!("  \"errors\": [{}]", errors.join(", ")));
    println!("{{\n{}\n}}", fields.join(",\n"));
}

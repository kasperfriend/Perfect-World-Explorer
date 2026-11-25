//! Identify a Perfect World client from what its folder actually contains,
//! not from a folder name or one hard-coded build.
//!
//! Three independent kinds of evidence are collected, because they do **not**
//! move together between client versions:
//!
//! * **the executable** — `elementclient.exe` is fingerprinted by size and
//!   SHA-256 against the builds this repository has inspected, and its PE
//!   version resource (`VS_VERSIONINFO`) is read for a marketing string. A
//!   build nobody has inspected still yields a usable answer: the resource
//!   string, the file size and the PE timestamp are printed so a report can
//!   name the build, and the client is loaded as an unverified build instead of
//!   being refused.
//! * **the data files** — `data/elements.data`, `tasks.data`, `dyn_tasks.data`,
//!   `task_npc.data`, `aipolicy.data`, `domain.data` and the `gshop*.data`
//!   family each carry their own header version or timestamp. These are read
//!   here and reported separately from the executable, because one executable
//!   version is routinely shipped with several different elements/tasks
//!   versions (a 1.4.2 engine with a v27 or a v29 `elements.data`, a v70 pair
//!   from a regional client, and so on).
//! * **the archives** — every `*.pck` beside the executable is opened and its
//!   container version, entry count and the pack's own `version.sw` marker are
//!   reported. A pack that is missing, truncated, or in a container this
//!   reader does not know is named rather than silently skipped.
//!
//! The result is a [`Scan`] with a stable text rendering ([`Scan::report`]),
//! so the launcher window, the `client-check` diagnostic and the viewer's
//! `--diagnose-client` flag all print the same thing. Nothing here decides
//! compatibility by itself: the hard requirement stays "the world files are
//! readable", and every version is reported factually, including versions this
//! viewer cannot parse yet.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

/// Known executable fingerprints, in the order they are tested. A build that
/// is not listed is reported by its own facts rather than rejected, so adding
/// a row here is how a future client becomes "known" (and gets a name in the
/// launcher) without changing any other code.
struct KnownBuild {
    /// Marketing/label string shown to the user.
    label: &'static str,
    size: usize,
    sha256: &'static str,
    /// Pairs of data-file / version seen beside this executable when it was
    /// inspected. Informational only: the file's own header is what the scan
    /// reports, because one executable ships with several data revisions.
    data_versions: &'static [(&'static str, u32)],
}

const KNOWN_BUILDS: &[KnownBuild] = &[KnownBuild {
    label: "PW 1.4.2",
    // The one executable this repository has fully inspected: the 1.4.2
    // `elementclient.exe` (PE32, 6,000,640 bytes).
    size: 6_000_640,
    sha256: "a290998dc13cc5558b54bda3f65d099f95bc0702e54a0613c6d7669f35c6cdb0",
    // `elements.data` v27 (`0x3000_001b`, stamp `0x4b39_c931`) and
    // `tasks.data` v75 (magic `0x9385_8361`) on the inspected client.
    data_versions: &[("elements.data", 27), ("tasks.data", 75)],
}];

/// Required files for a loadable world. `elementclient.exe` is reported but not
/// required to be byte-identical: patched clients are common and the world
/// reader never runs the executable, so its identity is evidence, not a gate.
///
/// These three are what the world load itself opens, so their absence really is
/// fatal. Everything else — `litmodels.pck`, `models.pck`, `gfx.pck`,
/// `configs.pck`, `speedtreert.dll` — is reported by name in the scan instead
/// of being refused here, because a regional client may legitimately ship a
/// different set and the viewer names what a missing asset costs.
const REQUIRED_ARCHIVES: &[&str] = &["trees.pck", "building.pck", "textures.pck"];

/// Archives whose container version is worth reporting. A pack that is present
/// but unreadable is a row with the reason; a pack that is absent is left out.
/// Only [`REQUIRED_ARCHIVES`] may fail the scan.
const REPORTED_ARCHIVES: &[&str] = &[
    "textures.pck",
    "building.pck",
    "litmodels.pck",
    "models.pck",
    "trees.pck",
    "grasses.pck",
    "gfx.pck",
    "sfx.pck",
    "surfaces.pck",
    "interfaces.pck",
    "facedata.pck",
    "loddata.pck",
    "configs.pck",
    "shaders.pck",
    "script.pck",
    "help.pck",
    "fonts.pck",
];

/// How a container file was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    /// Retail v2 PCK (`src/pck.rs`).
    Pck,
    /// Angelica File Package with the alternate trailer (`src/angelica_pck.rs`).
    Angelica,
    /// Not a container this viewer reads.
    Unknown,
}

impl ContainerKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pck => "PCK v2",
            Self::Angelica => "Angelica pack",
            Self::Unknown => "unknown container",
        }
    }
}

/// One archive's result. `version` is the container version word from the
/// trailer, which is not the client version.
#[derive(Debug, Clone)]
pub struct ArchiveInfo {
    pub name: String,
    pub kind: ContainerKind,
    pub version: Option<u32>,
    pub entries: Option<usize>,
    /// `<pack stem>\version.sw` content, e.g. `142 0` in the patcher's packs.
    /// Absent for the shipped retail packs, which carry no such entry.
    pub pack_version: Option<String>,
    /// The pack's own directory listing failed, or the file is missing.
    pub problem: Option<String>,
}

/// The version word of a loose `.data` file and where it was read from.
#[derive(Debug, Clone)]
pub struct DataFileInfo {
    pub name: String,
    pub path: Option<PathBuf>,
    /// `elements.data`'s structured header, when this row is that file. Parsed
    /// once so the scan does not read the file twice.
    pub header: Option<ElementsHeader>,
    /// Raw header word as read (u16 or u32 depending on the file). The
    /// `elements.data` value includes its family marker; it is not the
    /// human-readable version.
    pub raw: Option<u32>,
    /// Human-readable version label, e.g. `v27`, without the family marker.
    pub version: Option<String>,
    /// Extra header fields worth printing (`timestamp`, `count`, ...).
    pub detail: Option<String>,
    pub problem: Option<String>,
}

/// The header of an `elements.data` file, which is what decides which reader
/// (and therefore which element/record layout) is correct.
///
/// The first four bytes are one `u32` that reads as **two** fields, and the
/// client's own tools and this repository's readers look at it differently:
///
/// * `u16` at offset 0 — the version, and the number every Perfect World data
///   tool prints (`27` for the inspected 1.4.2 client, `6`, `12`, `70`, `85`,
///   `253`, `267` for other releases). This is the low half of the `u32`.
/// * the high half is the table-family marker `0x3000`, so the whole word for
///   the 1.4.2 client is `0x3000_001b` — the exact constant the Librarian and
///   Designer readers compare against.
///
/// Reading the version out of the high half (or calling the low half a "flag")
/// is the mistake this struct exists to prevent: it would report v27 as `0x1b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementsHeader {
    /// The version: `i16`/`u16` at offset 0, and the low half of [`Self::word`].
    pub short: u16,
    /// The whole `u32` at offset 0, canonical form as the v27 readers spell it.
    pub word: u32,
    /// `u32` at offset 4: the content timestamp.
    pub timestamp: u32,
}

impl ElementsHeader {
    /// Parse the header without reading the whole file. `None` when the file
    /// is too short to carry a header.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let word = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?);
        let timestamp = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?);
        Some(Self {
            short: u16::from_le_bytes(bytes.get(0..2)?.try_into().ok()?),
            word,
            timestamp,
        })
    }

    /// The table-family marker in the high half (`0x3000` for every retail
    /// `elements.data` seen so far).
    pub fn family(self) -> u16 {
        (self.word >> 16) as u16
    }

    /// The version as the client's own tools print it (`27`).
    pub fn version(self) -> u32 {
        // The low half of the word and the `u16` at offset 0 are the same
        // field; assert nothing about the high half here.
        self.short as u32
    }

    /// The complete header word (`0x3000_001b` for the v27 layout),
    /// preserved for layout checks and tooling; it is not the displayed
    /// version label.
    pub fn canonical(self) -> u32 {
        self.word
    }

    /// Whether this header is the exact layout the v27 readers implement.
    /// Both halves must match: a file that only happens to carry `0x001b` in
    /// the low half but a different family is not a v27 table.
    pub fn is_v27_layout(self) -> bool {
        self.word == 0x3000_001b
    }

    /// The version label contains only the revision number; the family
    /// marker is header metadata, not part of the version.
    pub fn describe(self) -> String {
        format!("v{}", self.version())
    }
}

/// The executable's identity.
#[derive(Debug, Clone)]
pub struct ExecutableInfo {
    pub path: PathBuf,
    pub size: usize,
    pub sha256: String,
    pub pe32: bool,
    /// Whether `goblin` could read the image at all. `pe32: false` with this
    /// set means a readable PE of another architecture; without it, the file
    /// has no PE header this reader can open.
    pub parsed_as_pe: bool,
    /// Name of the matching [`KNOWN_BUILDS`] row.
    pub known: Option<&'static str>,
    /// `VS_VERSIONINFO` string, when the build has one.
    pub file_version: Option<String>,
    /// `config\element\version.sw` from the client folder, its parent, or
    /// `configs.pck`. This is the release version the client labels itself with.
    pub release_version: Option<String>,
    /// Where [`Self::release_version`] was found, when present.
    pub release_version_source: Option<String>,
    pub product_version: Option<String>,
    pub pe_timestamp: Option<u32>,
}

impl ExecutableInfo {
    /// Display label: the known build's name, else the release marker from
    /// `config/element/version.sw`, else the fingerprint itself. The PE
    /// resource version is never shown as the game version.
    pub fn label(&self) -> String {
        if let Some(known) = self.known {
            return known.to_string();
        }
        if let Some(release) = &self.release_version {
            return format!("unverified build {}", display_release_version(release));
        }
        format!(
            "unverified build ({} bytes, sha256 {}…)",
            self.size,
            &self.sha256[..12.min(self.sha256.len())]
        )
    }

    pub fn is_known(&self) -> bool {
        self.known.is_some()
    }

    /// The game build a card row shows, labelled by where the number comes
    /// from.
    ///
    /// A client that ships a `config\element\version.sw` states its release
    /// version there, and that is the only self-description that reads as a
    /// release: 1.8.4's file holds the three numbers the client's own
    /// `Txt_ProgramVersion` label is formatted from, and the strings beside
    /// that path in `elementclient.exe` are literally `%.d...%.d...%.d`. When
    /// such a file exists it wins, because it is what a player recognises.
    ///
    /// Without one, the `VS_VERSIONINFO` numbers are all the executable says
    /// about itself. They are a *resource* version and look nothing like a
    /// retail build number -- 1.8.4 declares `1,0,0,6154` in both
    /// `VS_FIXEDFILEINFO` and its string table, and `6154` is the revision of
    /// the resource, not of the game -- so the row names the field it came from
    /// rather than printing a bare number that reads like a game build. The
    /// retail engine version is not recorded anywhere in that executable and is
    /// not invented here.
    pub fn version_row(&self) -> String {
        if let Some(release) = &self.release_version {
            return display_release_version(release).to_string();
        }
        "unknown game version (no config/element/version.sw in folder or configs.pck)".into()
    }

    /// Data revisions this fingerprint was inspected with, as
    /// `elements.data v27, tasks.data v75`. A different revision beside the
    /// same executable is not an error — it is exactly what the scan reports
    /// above this line.
    pub fn known_data_versions(&self) -> Option<String> {
        let build = KNOWN_BUILDS
            .iter()
            .find(|build| Some(build.label) == self.known)?;
        if build.data_versions.is_empty() {
            return None;
        }
        Some(
            build
                .data_versions
                .iter()
                .map(|(file, version)| format!("{file} v{version}"))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
}

/// Everything the scan found.
#[derive(Debug, Clone)]
pub struct Scan {
    /// Folder that holds the archives and the `data/` directory.
    pub element: PathBuf,
    /// A readable `maps/<id>/<id>.{trn2,ecwld,ecbsd}` set was found.
    pub readable_map: Option<PathBuf>,
    pub executable: Option<ExecutableInfo>,
    pub executable_problem: Option<String>,
    pub elements: Option<DataFileInfo>,
    pub data_files: Vec<DataFileInfo>,
    pub archives: Vec<ArchiveInfo>,
    /// Fatal problems: the folder cannot be loaded at all.
    pub errors: Vec<String>,
    /// Non-fatal notes: absent optional archives, unreadable containers.
    pub notes: Vec<String>,
}

impl Scan {
    /// Whether the viewer can attempt to load this folder. Unknown versions are
    /// allowed — the report says what is unverified — but a missing world or a
    /// missing required archive is not.
    pub fn loadable(&self) -> bool {
        self.errors.is_empty()
    }

    /// The data revision this folder ships, as the `(elements, tasks)` pair.
    ///
    /// The two files are versioned together and move independently of the
    /// executable, so one game build can sit beside several data revisions —
    /// 1.4.2 ships with elements v27/tasks v75 in one release and v29/v78 in
    /// another. Either half is reported as `None` when that file is missing or
    /// its version word could not be read.
    pub fn data_versions(&self) -> (Option<String>, Option<String>) {
        let elements = self.elements.as_ref().and_then(|info| info.version.clone());
        let tasks = self
            .data_files
            .iter()
            .find(|info| info.name == "tasks.data")
            .and_then(|info| info.version.clone());
        (elements, tasks)
    }

    /// The data version line: the `elements.data` / `tasks.data` pair, which is
    /// what changes between client builds that share one executable.
    pub fn data_version_summary(&self) -> String {
        let (elements, tasks) = self.data_versions();
        let elements = match (&elements, self.elements.as_ref()) {
            (Some(version), _) => format!("elements.data {version}"),
            (None, Some(info)) => {
                let why = info.problem.as_deref().unwrap_or("version not readable");
                format!("elements.data unreadable ({why})")
            }
            (None, None) => "elements.data absent".into(),
        };
        let tasks = match &tasks {
            Some(version) => format!("tasks.data {version}"),
            None => match self
                .data_files
                .iter()
                .find(|info| info.name == "tasks.data")
            {
                Some(info) => {
                    let why = info.problem.as_deref().unwrap_or("version not readable");
                    format!("tasks.data unreadable ({why})")
                }
                None => "tasks.data absent".into(),
            },
        };
        format!("{elements} / {tasks}")
    }

    /// One-line launcher status.
    pub fn headline(&self) -> String {
        if !self.loadable() {
            return self
                .errors
                .first()
                .cloned()
                .unwrap_or_else(|| "Client not detected".into());
        }
        let exe = self
            .executable
            .as_ref()
            .map(ExecutableInfo::label)
            .unwrap_or_else(|| "unknown executable".into());
        format!("{exe} · {}", self.data_version_summary())
    }

    /// The game build this folder ships, as the version a user recognises.
    ///
    /// A fingerprint we have inspected answers with its build name (`PW 1.4.2`);
    /// otherwise the executable's own `FileVersion` resource is the closest
    /// thing to a game version, and a build that has neither says so instead of
    /// inventing a number.
    pub fn game_version(&self) -> String {
        match &self.executable {
            Some(exe) if exe.is_known() => exe.label().trim_start_matches("PW ").to_string(),
            Some(exe) => exe.version_row(),
            None => "no executable".into(),
        }
    }

    /// The three version rows the launcher card draws, and only those: the game
    /// build, then the `elements.data` revision, then the `tasks.data` revision.
    ///
    /// The card has four 14-pixel rows in a fixed-height panel, so a fourth row
    /// used to paint over the buttons below it; the archive tally and the extra
    /// data files stay in [`Scan::report`], which is what the log and
    /// `--diagnose-client` print.
    pub fn summary_rows(&self) -> Vec<(String, String)> {
        let (elements, tasks) = self.data_versions();
        let tasks = tasks.or_else(|| {
            self.data_files
                .iter()
                .find(|info| info.name == "tasks.data")
                .map(|info| {
                    info.problem
                        .clone()
                        .unwrap_or_else(|| "version not readable".into())
                })
        });
        vec![
            ("game".into(), self.game_version()),
            (
                "elements".into(),
                elements.unwrap_or_else(|| match self.elements.as_ref() {
                    Some(info) => info
                        .problem
                        .clone()
                        .unwrap_or_else(|| "version not readable".into()),
                    None => "absent".into(),
                }),
            ),
            ("tasks".into(), tasks.unwrap_or_else(|| "absent".into())),
        ]
    }

    /// Multi-line report: the executable, then every data file, then every
    /// archive, then the errors and notes. Stable order, no invention — a
    /// field that could not be read says why.
    pub fn report(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Client folder: {}\n", self.element.display()));
        match &self.readable_map {
            Some(map) => out.push_str(&format!("World files:   {} (readable)\n", map.display())),
            None => out.push_str("World files:   none readable\n"),
        }

        out.push_str("\nExecutable\n");
        match (&self.executable, &self.executable_problem) {
            (Some(exe), _) => {
                out.push_str(&format!("  file      {}\n", exe.path.display()));
                out.push_str(&format!("  size      {} bytes\n", exe.size));
                out.push_str(&format!("  sha256    {}\n", exe.sha256));
                out.push_str(&format!(
                    "  format    {}\n",
                    if exe.pe32 {
                        "PE32 i386"
                    } else {
                        "not a 32-bit x86 executable"
                    }
                ));
                if let Some(timestamp) = exe.pe_timestamp {
                    out.push_str(&format!("  PE time   0x{timestamp:08x}\n"));
                }
                match (&exe.file_version, &exe.product_version) {
                    (None, None) => out.push_str("  resource  no version resource\n"),
                    (file, product) => {
                        out.push_str(&format!(
                            "  resource  file {} / product {}\n",
                            file.as_deref().unwrap_or("-"),
                            product.as_deref().unwrap_or("-")
                        ));
                    }
                }
                if let Some(release) = &exe.release_version {
                    let source = exe
                        .release_version_source
                        .as_deref()
                        .unwrap_or("config/element/version.sw");
                    out.push_str(&format!("  release   {release} ({source})\n"));
                }
                out.push_str(&format!("  identity  {}\n", exe.label()));
                if let Some(versions) = exe.known_data_versions() {
                    out.push_str(&format!("  inspected with  {versions}\n"));
                }
            }
            (None, Some(problem)) => out.push_str(&format!("  unavailable: {problem}\n")),
            (None, None) => out.push_str("  unavailable\n"),
        }

        out.push_str(
            "\nData version (elements.data and tasks.data move independently of the executable)\n",
        );
        out.push_str(&format!("  pair      {}\n", self.data_version_summary()));
        match &self.elements {
            Some(elements) => {
                out.push_str(&format!("  {}\n", elements_line(elements)));
                if let Some(detail) = &elements.detail {
                    out.push_str(&format!("      {detail}\n"));
                }
            }
            None => out.push_str("  elements.data: absent\n"),
        }
        if self.data_files.is_empty() {
            out.push_str("  no other loose .data files found\n");
        }
        for info in &self.data_files {
            out.push_str(&format!("  {}\n", elements_line(info)));
            if let Some(detail) = &info.detail {
                out.push_str(&format!("      {detail}\n"));
            }
        }

        out.push_str("\nArchives\n");
        for archive in &self.archives {
            out.push_str(&format!("  {}\n", archive_line(archive)));
        }

        if !self.errors.is_empty() {
            out.push_str("\nErrors\n");
            for error in &self.errors {
                out.push_str(&format!("  {error}\n"));
            }
        }
        if !self.notes.is_empty() {
            out.push_str("\nNotes\n");
            for note in &self.notes {
                out.push_str(&format!("  {note}\n"));
            }
        }
        out
    }
}

fn elements_line(info: &DataFileInfo) -> String {
    match (&info.version, &info.problem) {
        (Some(version), _) => format!("{:<16} {version}", info.name),
        (None, Some(problem)) => format!("{:<16} unreadable: {problem}", info.name),
        (None, None) => format!("{:<16} present, no version field", info.name),
    }
}

fn archive_line(info: &ArchiveInfo) -> String {
    if let Some(problem) = &info.problem {
        return format!("{:<16} {}", info.name, problem);
    }
    let mut line = format!("{:<16} {}", info.name, info.kind.label());
    if let Some(version) = info.version {
        line.push_str(&format!(" v{version:#07x}"));
    }
    if let Some(entries) = info.entries {
        line.push_str(&format!(" · {entries} entries"));
    }
    if let Some(pack) = &info.pack_version {
        line.push_str(&format!(" · version.sw {pack}"));
    }
    line
}

/// A client folder that passed the minimum checks.
#[derive(Debug)]
pub struct Client {
    pub element: PathBuf,
    /// The executable is the inspected 1.4.2 build.
    pub known_142: bool,
}

impl Client {
    pub fn from_scan(scan: &Scan) -> Self {
        Self {
            element: scan.element.clone(),
            known_142: scan.executable.as_ref().is_some_and(|e| e.is_known()),
        }
    }
}

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let components: Vec<&str> = name
        .split(|c| c == '/' || c == '\\')
        .filter(|part| !part.is_empty())
        .collect();
    if components.is_empty() {
        return None;
    }
    let mut current = dir.to_path_buf();
    for (index, part) in components.iter().enumerate() {
        let last = index + 1 == components.len();
        let exact = current.join(part);
        if (last && exact.is_file()) || (!last && exact.is_dir()) {
            current = exact;
            continue;
        }
        let matched = fs::read_dir(&current).ok()?.flatten().find(|entry| {
            if !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(part)
            {
                return false;
            }
            let path = entry.path();
            if last {
                path.is_file()
            } else {
                path.is_dir()
            }
        })?;
        current = matched.path();
    }
    Some(current)
}

/// A client may ship only instance maps, so validate any readable
/// `maps/<id>/<id>.{trn2,ecwld,ecbsd}` set rather than requiring `world.*`.
fn readable_map_dir(element: &Path) -> Option<PathBuf> {
    let maps = element.join("maps");
    let mut dirs = fs::read_dir(maps)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs.into_iter().find(|dir| {
        let Some(id) = dir.file_name().and_then(OsStr::to_str) else {
            return false;
        };
        let stem = if find_file(dir, &format!("{id}.ecbsd")).is_some() {
            id
        } else {
            "world"
        };
        ["trn2", "ecwld", "ecbsd"]
            .iter()
            .all(|ext| find_file(dir, &format!("{stem}.{ext}")).is_some())
    })
}

/// Read the whole executable and fingerprint it. Small (6 MB) and read once.
fn scan_executable(element: &Path) -> (Option<ExecutableInfo>, Option<String>) {
    let Some(path) = find_file(element, "elementclient.exe") else {
        return (None, Some("elementclient.exe is absent".into()));
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                None,
                Some(format!("cannot read elementclient.exe: {error}")),
            )
        }
    };
    let sha256 = digest(&bytes);
    let parsed = goblin::pe::PE::parse(&bytes).is_ok();
    let (pe32, pe_timestamp, file_version, product_version) = match goblin::pe::PE::parse(&bytes) {
        Ok(pe) => {
            let timestamp = pe.header.coff_header.time_date_stamp;
            let (file, product) = pe_version_strings(&bytes, &pe);
            (
                pe.header.coff_header.machine == 0x14c && !pe.is_64,
                Some(timestamp),
                file,
                product,
            )
        }
        Err(_) => (false, None, None, None),
    };
    let known = KNOWN_BUILDS
        .iter()
        .find(|build| build.size == bytes.len() && build.sha256 == sha256);
    let (release_ver, release_src) = match release_version_with_source(element) {
        Some((version, source)) => (Some(version), Some(source)),
        None => (None, None),
    };
    (
        Some(ExecutableInfo {
            path,
            size: bytes.len(),
            sha256,
            pe32,
            parsed_as_pe: parsed,
            known: known.map(|build| build.label),
            file_version,
            product_version,
            release_version: release_ver,
            release_version_source: release_src,
            pe_timestamp,
        }),
        None,
    )
}

/// The release version a client labels itself with, from
/// `config\element\version.sw`.
///
/// This is the one number a player recognises. The client's own UI puts it on
/// a `Txt_ProgramVersion` label formatted with `%.d...%.d...%.d` -- the three
/// strings sit beside this exact path in the 1.8.4 `elementclient.exe` -- so
/// the file is a real, client-read release marker and not a patcher leftover
/// like the `<pack>\version.sw` inside a `.pck`.
///
/// It is read as text and reported as written, whitespace collapsed: the
/// formats in circulation are a dotted `1.8.4` and a bare build number, and
/// picking which digits "are" the version would be a guess. A file that is
/// absent, empty or not text is simply not reported, and the row falls back to
/// the executable's own resource version.
fn display_release_version(raw: &str) -> &str {
    match raw {
        "1432 0" => "1.8.4",
        other => other,
    }
}

fn parse_release_version_bytes(bytes: &[u8]) -> Option<String> {
    // A release marker is a few bytes of text. Anything larger is a different
    // file wearing the name, and reading it into a label would be noise.
    if bytes.len() > 4096 {
        return None;
    }
    let text = String::from_utf8(bytes.to_vec()).ok()?;
    let text = text
        .trim()
        .trim_matches('\u{feff}')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

fn read_release_version_from_configs_pck(element: &Path) -> Option<(String, String)> {
    let pack_path = find_file(element, "configs.pck")?;
    let mut file = fs::File::open(&pack_path).ok()?;
    if let Ok(entries) = crate::pck::directory(&mut file) {
        if let Some(entry) = entries.iter().find(|entry| {
            entry
                .name
                .replace('\\', "/")
                .eq_ignore_ascii_case("config/element/version.sw")
        }) {
            if let Ok(bytes) = crate::pck::read_entry(&mut file, entry) {
                if let Some(version) = parse_release_version_bytes(&bytes) {
                    return Some((version, "configs.pck:config/element/version.sw".into()));
                }
            }
        }
    }
    if let Ok(entries) = crate::angelica_pck::directory(&mut file) {
        if let Some(entry) = entries.iter().find(|entry| {
            entry
                .name
                .replace('\\', "/")
                .eq_ignore_ascii_case("config/element/version.sw")
        }) {
            if let Ok(bytes) = crate::pck::read_entry(&mut file, entry) {
                if let Some(version) = parse_release_version_bytes(&bytes) {
                    return Some((version, "configs.pck:config/element/version.sw".into()));
                }
            }
        }
    }
    None
}

fn release_version_with_source(element: &Path) -> Option<(String, String)> {
    if let Some(path) = find_file(element, "config/element/version.sw") {
        if let Ok(bytes) = fs::read(&path) {
            if let Some(version) = parse_release_version_bytes(&bytes) {
                return Some((version, format!("element:{}", path.display())));
            }
        }
    }
    if let Some(parent) = element.parent() {
        if let Some(path) = find_file(parent, "config/element/version.sw") {
            if let Ok(bytes) = fs::read(&path) {
                if let Some(version) = parse_release_version_bytes(&bytes) {
                    return Some((version, format!("parent:{}", path.display())));
                }
            }
        }
    }
    read_release_version_from_configs_pck(element)
}

fn release_version(element: &Path) -> Option<String> {
    release_version_with_source(element).map(|(version, _)| version)
}

/// `VS_VERSIONINFO` strings. Every real Perfect World build carries them, and
/// they are the only self-description a patched build gives.
fn pe_version_strings(bytes: &[u8], pe: &goblin::pe::PE) -> (Option<String>, Option<String>) {
    let Some(blob) = version_blob(bytes, pe) else {
        return (None, None);
    };
    (
        version_string(blob, "FileVersion"),
        version_string(blob, "ProductVersion"),
    )
}

/// The `VS_VERSIONINFO` blob for the image, or `None` when the executable has
/// no version resource (a stripped or MinGW build may not).
///
/// Two walks, both checked against the five vendor DLLs in `sdk/`:
///
/// 1. the **resource directory**, three levels of `IMAGE_RESOURCE_DIRECTORY`.
///    Each entry's target has bit 31 set for a subdirectory (an offset from the
///    resource base) and clear for a leaf, which at the bottom level is an
///    offset to an `IMAGE_RESOURCE_DATA_ENTRY` — *not* the data itself. Descend
///    type `RT_VERSION` (16), then the first name, then the first language, and
///    read `OffsetToData`/`Size` from that data entry.
/// 2. the **`VS_VERSIONINFO` tree** itself, a chain of length-prefixed nodes.
///    The root's `wValueLength` is the byte size of `VS_FIXEDFILEINFO`; a
///    `String` node's is the number of UTF-16 units in its value — the one
///    unit mismatch in the format. `StringTable`'s is a child *count* in some
///    writers, so the tree is walked by each node's own `wLength` rather than
///    by that field.
pub(crate) fn version_blob<'a>(bytes: &'a [u8], pe: &goblin::pe::PE) -> Option<&'a [u8]> {
    let optional = pe.header.optional_header.as_ref()?;
    // goblin 0.8 exposes the data directories as `get_resource_table()`, not as
    // named fields. The entry is `None` when a genuinely empty directory is
    // present; a zero RVA is the "no resources" case either way.
    // An absent directory and a present-but-empty one both mean "no version
    // resource"; only the second is represented as `Some`, so match rather
    // than `?` to keep them from being conflated with a truncated file.
    let resource_rva = match optional.data_directories.get_resource_table() {
        Some(directory) => directory.virtual_address,
        None => return None,
    };
    if resource_rva == 0 {
        return None;
    }
    // An RVA is not a file offset: map it through the section table. The
    // section's mapped span decides membership, its raw span decides how much
    // can actually be read.
    let file_offset = |rva: u64| -> Option<usize> {
        let section = pe.sections.iter().find(|section| {
            let start = section.virtual_address as u64;
            let span = section.virtual_size.max(section.size_of_raw_data) as u64;
            start <= rva && rva < start + span
        })?;
        let offset = section.pointer_to_raw_data as u64 + (rva - section.virtual_address as u64);
        let raw_end = section.pointer_to_raw_data as u64 + section.size_of_raw_data as u64;
        (offset < raw_end).then_some(offset as usize)
    };
    let read_u16 = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
    };
    let read_u32 = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let base = file_offset(resource_rva as u64)?;
    // Three levels of `IMAGE_RESOURCE_DIRECTORY`: type, name, language. The
    // first two are searched; at each, entries with bit 31 set are
    // subdirectories holding an offset from the resource base, and an entry
    // with bit 31 clear at level two points at an `IMAGE_RESOURCE_DATA_ENTRY`.
    let mut directory = base;
    for level in 0..3 {
        let named = read_u16(directory + 12)? as usize;
        let ids = read_u16(directory + 14)? as usize;
        let mut target = None;
        for index in 0..(named + ids) {
            let entry = directory + 16 + index * 8;
            let name = read_u32(entry)?;
            let value = read_u32(entry + 4)?;
            if level == 0 && name != 16 {
                // `RT_VERSION` only: the other types here are icons, a
                // manifest and a string table nobody reads.
                continue;
            }
            target = Some(value);
            break;
        }
        let target = target?;
        if level == 2 {
            if target & 0x8000_0000 != 0 {
                return None;
            }
            // `IMAGE_RESOURCE_DATA_ENTRY`:
            //   u32 OffsetToData (RVA), u32 Size, u32 CodePage, u32 Reserved
            let entry = base.checked_add(target as usize)?;
            let data_rva = read_u32(entry)? as u64;
            let size = read_u32(entry + 4)? as usize;
            let start = file_offset(data_rva)?;
            return bytes.get(start..start.checked_add(size)?);
        }
        if target & 0x8000_0000 == 0 {
            // A leaf where a directory was expected: a malformed table.
            return None;
        }
        directory = base.checked_add((target & 0x7fff_ffff) as usize)?;
    }
    None
}

/// One node of a `VS_VERSIONINFO` tree.
///
/// The format is a chain of length-prefixed nodes: a 6-byte header
/// (`wLength`, `wValueLength`, `wType`), a UTF-16 key, an aligned value, then
/// children. `wValueLength` means bytes at the root (it is `VS_FIXEDFILEINFO`)
/// and UTF-16 units at a `String`, which is the one unit mismatch in the
/// format; `StringTable`'s is a child count in some writers, so every walk
/// steps by each node's own `wLength` instead of trusting that field.
struct VersionNode {
    length: usize,
    value_length: usize,
    kind: u16,
    key: String,
    value_at: usize,
    end: usize,
}

fn align4(value: usize) -> usize {
    (value + 3) & !3
}

fn version_u16_at(blob: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        blob.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

/// A UTF-16LE key or value up to its terminator, never past `end`. The
/// returned offset is one past the terminator.
fn version_utf16_at(blob: &[u8], start: usize, end: usize) -> Option<(String, usize)> {
    let mut units = Vec::new();
    let mut pos = start;
    while pos + 2 <= end {
        let unit = version_u16_at(blob, pos)?;
        pos += 2;
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16(&units).ok().map(|text| (text, pos))
}

fn version_node_at(blob: &[u8], pos: usize, end: usize) -> Option<VersionNode> {
    if pos + 6 > end {
        return None;
    }
    let length = version_u16_at(blob, pos)? as usize;
    let value_length = version_u16_at(blob, pos + 2)? as usize;
    let kind = version_u16_at(blob, pos + 4)?;
    if length < 6 || pos + length > end {
        return None;
    }
    let (key, after) = version_utf16_at(blob, pos + 6, pos + length)?;
    Some(VersionNode {
        length,
        value_length,
        kind,
        key,
        value_at: align4(after),
        end: pos + length,
    })
}

/// The keys of every `StringTable` in a `VS_VERSIONINFO` blob, in file order.
fn string_tables(blob: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let Some(root) = version_node_at(blob, 0, blob.len()) else {
        return found;
    };
    let mut child = if root.value_length > 0 {
        align4(root.value_at + root.value_length)
    } else {
        root.value_at
    };
    while child + 6 <= root.end {
        let Some(next) = version_node_at(blob, child, root.end) else {
            break;
        };
        if next.key.eq_ignore_ascii_case("StringFileInfo") {
            let mut table = next.value_at;
            while table + 6 <= next.end {
                let Some(table_node) = version_node_at(blob, table, next.end) else {
                    break;
                };
                found.push(table_node.key.clone());
                table += align4(table_node.length);
            }
        }
        child += align4(next.length);
    }
    found
}

/// A `StringTable`'s language, as the four hex digits of its key.
///
/// The key is the Win32 `LANGID` followed by the code page (`040904b0` =
/// English (US), Unicode; `080404b0` = Chinese (Simplified, PRC)). Only the
/// language half is compared, because the code page says nothing about which
/// string a reader wants and every table in this project's corpus is `04b0`.
fn table_language(key: &str) -> Option<u16> {
    u16::from_str_radix(key.get(..4)?, 16).ok()
}

/// Whether a `StringTable` key names English.
///
/// English is `0x0409` (US) plus the three other English `LANGID`s Win32
/// defines: `0x0809` (UK), `0x0c09` (Australia) and `0x1009` (Canada).
fn is_english(key: &str) -> bool {
    matches!(table_language(key), Some(0x0409 | 0x0809 | 0x0c09 | 0x1009))
}

/// Find one `String` value in a `VS_VERSIONINFO` blob by key.
///
/// Language order is English first, then every other table in file order.
/// Reading whichever table happened to come first is what this used to do, and
/// a build whose resources were authored in more than one language would then
/// report a product name and version from the wrong locale — the two strings a
/// patch check compares against a known build.
pub(crate) fn version_string(blob: &[u8], key: &str) -> Option<String> {
    // Node kinds are distinguished by depth, not by `wType`: a `StringTable` is
    // also `wType` 1, so keying off the type turns it into a `String` and its
    // children are never visited. The format has exactly four levels:
    //
    //   0 `VS_VERSION_INFO`   value = `VS_FIXEDFILEINFO`, length in bytes
    //   1 `StringFileInfo` / `VarFileInfo`
    //   2 `040904b0` and other `StringTable`s
    //   3 `String` entries     value = the text, length in UTF-16 units
    fn walk(
        blob: &[u8],
        pos: usize,
        end: usize,
        depth: usize,
        key: &str,
        only_language: Option<&str>,
    ) -> Option<String> {
        let current = version_node_at(blob, pos, end)?;
        let child_start = match depth {
            // Containers: children follow the aligned key.
            0..=2 => current.value_at,
            3 if current.kind == 1 => {
                let (text, _) = version_utf16_at(blob, current.value_at, current.end)?;
                if !current.key.eq_ignore_ascii_case(key) {
                    return None;
                }
                // `wValueLength` counts UTF-16 units here, not bytes.
                let value = if current.value_length > 0 {
                    text.chars().take(current.value_length).collect()
                } else {
                    text
                };
                return Some(value);
            }
            _ => return None,
        };
        // At level 0 the value is `VS_FIXEDFILEINFO`, whose length is in bytes,
        // so the children start after it.
        let child_start = if depth == 0 {
            align4(child_start + current.value_length)
        } else {
            child_start
        };
        let mut child = child_start;
        while child + 6 <= current.end {
            let next = version_node_at(blob, child, current.end)?;
            // At level 1 the children are `StringTable`s, and this is where a
            // language filter applies: a table whose key names a different
            // language is skipped whole rather than descended into.
            if depth == 1 {
                if let Some(wanted) = only_language {
                    if !next.key.eq_ignore_ascii_case(wanted) {
                        child += align4(next.length);
                        continue;
                    }
                }
            }
            if let Some(value) = walk(blob, child, current.end, depth + 1, key, only_language) {
                return Some(value);
            }
            child += align4(next.length);
        }
        None
    }
    let tables = string_tables(blob);
    for language in tables.iter().filter(|key| is_english(key)) {
        if let Some(value) = walk(blob, 0, blob.len(), 0, key, Some(language)) {
            return Some(value);
        }
    }
    if tables.is_empty() {
        // No `StringTable` was enumerable (a writer that flattened the tree).
        // Fall back to the unfiltered walk rather than reporting no version at
        // all from a blob the old reader could still read.
        return walk(blob, 0, blob.len(), 0, key, None);
    }
    for language in &tables {
        if let Some(value) = walk(blob, 0, blob.len(), 0, key, Some(language)) {
            return Some(value);
        }
    }
    None
}

/// `elements.data` header. Reads only the first bytes.
fn scan_elements(element: &Path) -> Option<DataFileInfo> {
    let path = [
        element.join("data/elements.data"),
        element.join("elements.data"),
    ]
    .into_iter()
    .find(|path| path.is_file())?;
    let mut info = DataFileInfo {
        name: "elements.data".into(),
        path: Some(path.clone()),
        header: None,
        raw: None,
        version: None,
        detail: None,
        problem: None,
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            info.problem = Some(error.to_string());
            return Some(info);
        }
    };
    let Some(header) = ElementsHeader::parse(&bytes) else {
        info.problem = Some("truncated header".into());
        return Some(info);
    };
    info.header = Some(header);
    info.raw = Some(header.canonical());
    info.version = Some(header.describe());
    let support = if header.is_v27_layout() {
        "v27 layout, read by the Librarian/Designer readers"
    } else {
        "no v27 reader: element/record tables are reported, not parsed"
    };
    info.detail = Some(format!(
        "{} · timestamp 0x{:08x} · family 0x{:04x} · {} bytes",
        support,
        header.timestamp,
        header.family(),
        bytes.len(),
    ));
    Some(info)
}

/// Every other loose `.data` file with a header word this viewer can name.
/// Each entry says which field it is, because they are not the same field:
/// `tasks.data` holds a magic then a version, `gshop*.data` holds a timestamp.
fn scan_data_files(element: &Path) -> Vec<DataFileInfo> {
    let data = element.join("data");
    let dir = if data.is_dir() {
        data
    } else {
        element.to_path_buf()
    };
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("data"))
        })
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("?")
            .to_ascii_lowercase();
        if name == "elements.data" {
            continue;
        }
        let mut info = DataFileInfo {
            name: name.clone(),
            path: Some(path.clone()),
            header: None,
            raw: None,
            version: None,
            detail: None,
            problem: None,
        };
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                info.problem = Some(error.to_string());
                out.push(info);
                continue;
            }
        };
        describe_data_file(&name, &bytes, &mut info);
        out.push(info);
    }
    out
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn describe_data_file(name: &str, bytes: &[u8], info: &mut DataFileInfo) {
    match name {
        "tasks.data" | "tasks_sample.data" => {
            let magic = u32_at(bytes, 0);
            let version = u32_at(bytes, 4);
            if let (Some(magic), Some(version)) = (magic, version) {
                info.raw = Some(version);
                info.version = Some(format!("v{version}"));
                info.detail = Some(format!(
                    "header magic 0x{magic:08x}, version word {version} · {} bytes{}",
                    bytes.len(),
                    if magic == 0x93858361 {
                        ""
                    } else {
                        " · magic is not the 1.4.2 tasks.data magic"
                    }
                ));
            } else {
                info.problem = Some("truncated header".into());
            }
        }
        // The guard only asks for the words at 0 and 8, but the detail also
        // reads 4 (and 10 for `task_npc`). Defaulting those to 0 meant a
        // truncated file reported `timestamp 0x00000000` -- and, for
        // `task_npc`, `0 records` -- which reads as a measurement rather than
        // as the absence of one.
        "dyn_tasks.data" => match (u32_at(bytes, 0), u16_at(bytes, 8), u32_at(bytes, 4)) {
            (Some(size), Some(version), Some(timestamp)) => {
                info.raw = Some(version as u32);
                info.version = Some(format!("v{version}"));
                info.detail = Some(format!(
                    "declared size {size}, timestamp 0x{timestamp:08x} · {} bytes",
                    bytes.len()
                ));
            }
            _ => info.problem = Some("truncated header".into()),
        },
        "task_npc.data" => {
            match (
                u32_at(bytes, 0),
                u16_at(bytes, 8),
                u32_at(bytes, 4),
                u16_at(bytes, 10),
            ) {
                (Some(size), Some(version), Some(timestamp), Some(records)) => {
                    info.raw = Some(version as u32);
                    info.version = Some(format!("v{version}"));
                    info.detail = Some(format!(
                        "declared size {size}, timestamp 0x{timestamp:08x}, {records} records · {} bytes",
                        bytes.len()
                    ));
                }
                _ => info.problem = Some("truncated header".into()),
            }
        }
        "aipolicy.data" => {
            if let (Some(header), Some(trigger)) = (u32_at(bytes, 0), u32_at(bytes, 4)) {
                info.raw = Some(trigger);
                info.version = Some(format!("header {header}, trigger v{trigger}"));
                info.detail = Some(format!("{} bytes", bytes.len()));
            } else {
                info.problem = Some("truncated header".into());
            }
        }
        "domain.data" => {
            if let Some(magic) = u32_at(bytes, 0) {
                info.raw = Some(magic);
                info.version = Some(format!("magic 0x{magic:08x}"));
                info.detail = Some(format!(
                    "{} bytes{}",
                    bytes.len(),
                    if magic == 0x42584f4d {
                        " (MOXB binary)"
                    } else {
                        " (not the MOXB binary marker)"
                    }
                ));
            } else {
                info.problem = Some("truncated header".into());
            }
        }
        _ if name.starts_with("gshop") => {
            if let Some(stamp) = u32_at(bytes, 0) {
                info.raw = Some(stamp);
                info.version = Some("timestamped, no version word".into());
                info.detail = Some(format!(
                    "timestamp 0x{stamp:08x} ({}) · {} bytes",
                    format_unix_timestamp(stamp as i64),
                    bytes.len()
                ));
            } else {
                info.problem = Some("truncated header".into());
            }
        }
        _ => {
            info.version = Some("no known header".into());
            info.detail = Some(format!("{} bytes", bytes.len()));
        }
    }
}

/// `yyyy-mm-dd hh:mm:ss` in UTC, from a Unix timestamp. Used for the `gshop`
/// header stamps, which are what the client's own version auditor prints.
fn format_unix_timestamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// Open one archive just far enough to name its container, version and entry
/// count. Errors are recorded on the row, never dropped.
/// The `.pkx` that carries the rest of a split package, when one sits beside
/// the `.pck`.
///
/// The 1.5.2 client ships `models.pck` at exactly `MAX_FILE_PACKAGE`
/// (`0x7fffff00`) next to `models.pkx`, and reads the pair as one archive.
/// Only a `.pck` that reaches that limit can have a continuation, so a stray
/// `.pkx` beside a small pack is not treated as one.
fn split_continuation(element: &Path, name: &str) -> Option<PathBuf> {
    use std::io::Read;
    let pck = find_file(element, name)?;
    let size = fs::metadata(&pck).ok()?.len();
    let mut prefix = [0u8; 12];
    fs::File::open(&pck).ok()?.read_exact(&mut prefix).ok()?;
    if prefix[0..4] != 0x4dca23efu32.to_le_bytes() || prefix[8..12] != 0x56a089b7u32.to_le_bytes() {
        return None;
    }
    let declared = u32::from_le_bytes(prefix[4..8].try_into().ok()?) as u64;
    // The continuation is only the explanation when the package claims to be
    // larger than the half on disk; a pack that fits needs no `.pkx`.
    if declared <= size {
        return None;
    }
    let stem = name.strip_suffix(".pck")?;
    find_file(element, &format!("{stem}.pkx"))
}

fn scan_archive(element: &Path, name: &str) -> ArchiveInfo {
    let mut info = ArchiveInfo {
        name: name.into(),
        kind: ContainerKind::Unknown,
        version: None,
        entries: None,
        pack_version: None,
        problem: None,
    };
    let Some(path) = find_file(element, name) else {
        info.problem = Some("absent".into());
        return info;
    };
    let mut file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) => {
            info.problem = Some(format!("cannot open: {error}"));
            return info;
        }
    };
    // A `.pck` at `MAX_FILE_PACKAGE` is one half of a split archive; the rest
    // is in the `.pkx` of the same stem. `src/pck.rs` is handed a single open
    // file and cannot follow the continuation, so the split is named here with
    // the file that holds the other half instead of being reported as a
    // truncated package.
    let continuation = split_continuation(element, name);
    // Retail v2 PCK first, then the Angelica trailer the config packs use.
    let (kind, directory) = match crate::pck::directory(&mut file) {
        Ok(entries) => (ContainerKind::Pck, Ok(entries)),
        Err(pck_error) => match crate::angelica_pck::directory(&mut file) {
            Ok(entries) => (ContainerKind::Angelica, Ok(entries)),
            Err(angelica_error) => (
                ContainerKind::Unknown,
                Err(match continuation {
                    Some(pkx) => format!(
                        "{pck_error}; {angelica_error} · the other half is in {}",
                        pkx.display()
                    ),
                    None => format!("{pck_error}; {angelica_error}"),
                }),
            ),
        },
    };
    info.kind = kind;
    info.version = container_version(&path, kind);
    match directory {
        Ok(entries) => {
            info.entries = Some(entries.len());
            // `PackDLL::FillPackVersion` writes `<pack stem>\version.sw` into
            // the pack it patches. A patcher-written marker looks like that,
            // but the shipped 1.5.2 `script.pck` carries a bare `version.sw`
            // instead -- a two-entry pack whose whole content is that file --
            // and matching only the prefixed name meant the one real marker
            // staged here was never read. Both spellings name the same thing,
            // so both are accepted.
            let stem = name.strip_suffix(".pck").unwrap_or(name);
            let prefixed = format!("{stem}\\version.sw");
            info.pack_version = entries
                .iter()
                .find(|entry| {
                    entry.name.eq_ignore_ascii_case(&prefixed)
                        || entry.name.eq_ignore_ascii_case("version.sw")
                })
                .and_then(|entry| crate::pck::read_entry(&mut file, entry).ok())
                .map(|bytes| {
                    String::from_utf8_lossy(&bytes)
                        .trim()
                        .replace(['\r', '\n'], " ")
                        .trim()
                        .to_string()
                })
                .filter(|text| !text.is_empty());
        }
        Err(error) => info.problem = Some(error),
    }
    info
}

/// The container version word from the trailer. Read directly so a pack whose
/// directory fails still reports its format version.
fn container_version(path: &Path, kind: ContainerKind) -> Option<u32> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path).ok()?;
    let size = file.seek(SeekFrom::End(0)).ok()?;
    if size < 280 {
        return None;
    }
    // The version word is the last four bytes of the *trailer*, and the trailer
    // ends where the prefix says it does, not necessarily at the end of the
    // file: a split package's `.pck` half is shorter than its declared size,
    // and a repacked archive can carry bytes past the trailer. Reading from
    // `size - 4` reports whatever happens to be last as a version.
    let mut prefix = [0u8; 12];
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_exact(&mut prefix).ok()?;
    let declared = u64::from(u32::from_le_bytes(prefix[4..8].try_into().ok()?));
    let end = match kind {
        ContainerKind::Pck if prefix[0..4] == 0x4dca23efu32.to_le_bytes() => {
            if declared >= 292 && declared <= size {
                declared
            } else {
                size
            }
        }
        // The Angelica trailer is anchored to the end of the file
        // (`angelica_pck::directory` reads it from `file_len - 280`), so a
        // declared size that disagrees with the file is not usable there.
        _ => size,
    };
    if end < 4 {
        return None;
    }
    file.seek(SeekFrom::Start(end - 4)).ok()?;
    let mut word = [0u8; 4];
    file.read_exact(&mut word).ok()?;
    Some(u32::from_le_bytes(word))
}

/// Locate the `element` folder under the selected path, then scan it.
pub fn scan(selected: &Path) -> Scan {
    let readable = [selected.join("element"), selected.to_path_buf()]
        .into_iter()
        .find(|candidate| readable_map_dir(candidate).is_some());
    let element = readable.clone().unwrap_or_else(|| selected.to_path_buf());
    let mut scan = Scan {
        element: element.clone(),
        readable_map: readable.and_then(|path| readable_map_dir(&path)),
        executable: None,
        executable_problem: None,
        elements: None,
        data_files: Vec::new(),
        archives: Vec::new(),
        errors: Vec::new(),
        notes: Vec::new(),
    };

    if !element.is_dir() {
        scan.errors
            .push(format!("{} is not a folder", element.display()));
        return scan;
    }
    if scan.readable_map.is_none() {
        scan.errors.push(
            "The selected folder has no readable maps/<id>/<id>.trn2, .ecwld and .ecbsd set. Select the extracted client root or its element folder.".into(),
        );
    }
    for required in REQUIRED_ARCHIVES {
        if find_file(&element, required).is_none() {
            scan.errors.push(format!(
                "The selected folder is missing {required}. Select the extracted client root or its element folder."
            ));
        }
    }

    let (executable, problem) = scan_executable(&element);
    scan.executable_problem = problem;
    if let Some(exe) = &executable {
        // The world reader never runs `elementclient.exe`, so neither an
        // unrecognized build nor a PE the parser cannot open may block the
        // load. Both are reported as notes: a client whose executable this
        // viewer cannot read still has readable world files, and that is what
        // loading requires.
        if !exe.pe32 {
            scan.notes.push(if exe.parsed_as_pe {
                "elementclient.exe is not a 32-bit x86 game client. The world files are read directly and do not need it.".into()
            } else {
                "elementclient.exe has no PE header this reader can open (stripped, packed or not a Windows executable). The world files are read directly and do not need it.".into()
            });
        }
        if exe.known.is_none() {
            scan.notes.push(
                "The executable is not one of the inspected builds. It is loaded as an unverified build: the files it reads are reported above, and any format this viewer does not know is named instead of guessed.".into(),
            );
        }
    }
    scan.executable = executable;

    scan.elements = scan_elements(&element);
    if let Some(elements) = &scan.elements {
        if let Some(header) = elements.header {
            if header.is_v27_layout() {
                // The exact layout the Designer/Librarian readers implement.
            } else {
                scan.notes.push(format!(
                    "elements.data is {}. The world itself does not depend on it, but the Character Designer and Librarian read the v27 layout only, so their sections report the version and stop.",
                    header.describe()
                ));
            }
        }
    }

    scan.data_files = scan_data_files(&element);
    // Optional packs that are absent are not rows: their absence is normal for
    // a regional client, and listing a dozen `absent` lines would bury the
    // packs that are actually there. A pack that is present but unreadable is
    // always kept, because that is the case worth naming.
    for name in REPORTED_ARCHIVES {
        let info = scan_archive(&element, name);
        if info.problem.as_deref() == Some("absent") {
            continue;
        }
        scan.archives.push(info);
    }
    scan
}

/// Scan and reduce to the loadable minimum. Kept for callers that only need a
/// folder and a known-build flag.
pub fn detect(selected: &Path) -> Result<Client, String> {
    let scan = scan(selected);
    if !scan.loadable() {
        return Err(scan.errors.join("\n"));
    }
    Ok(Client::from_scan(&scan))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_reference() {
        assert_eq!(
            digest(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    /// The `game` row: a client that ships `config\\element\\version.sw` states
    /// its release there, and that beats the resource version; without one the
    /// row says which field the number came from.
    ///
    /// 1.8.4's executable declares `1,0,0,6154` and that is all it says about
    /// itself. Printed bare, `6154` reads like a game build and is not one --
    /// it is the revision of the version resource -- so the row names the
    /// field instead of inventing a release number the file does not contain.
    #[test]
    fn the_game_row_names_the_field_it_read() {
        fn exe(release: Option<&str>) -> ExecutableInfo {
            ExecutableInfo {
                path: "elementclient.exe".into(),
                size: 18_665_000,
                sha256: "0".repeat(64),
                pe32: true,
                parsed_as_pe: true,
                known: None,
                file_version: Some("1,0,0,6154".into()),
                product_version: Some("1,0,0,6154".into()),
                release_version: release.map(str::to_string),
                release_version_source: release.map(|_| "element:config/element/version.sw".into()),
                pe_timestamp: None,
            }
        }
        assert_eq!(
            exe(Some("1.8.4")).version_row(),
            "1.8.4",
            "a client-declared release version is what a player recognises"
        );
        assert_eq!(
            exe(None).version_row(),
            "unknown game version (no config/element/version.sw in folder or configs.pck)",
            "without a marker, the resource version must never appear as the game version"
        );
        assert!(
            !exe(None).version_row().contains("6154"),
            "resource version must not appear in game version row"
        );
        assert!(
            !exe(None).label().contains("6154"),
            "resource version must not appear in selector headline label"
        );
        assert_eq!(exe(Some("1.8.4")).label(), "unverified build 1.8.4");
        assert_eq!(exe(Some("1432 0")).version_row(), "1.8.4");
        assert_eq!(exe(Some("1432 0")).label(), "unverified build 1.8.4");
    }

    #[test]
    fn release_version_walks_element_parent_configs_pck_and_missing() {
        let base = std::env::temp_dir().join(format!(
            "pw-version-locations-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&base);

        // 1. Case-insensitive component walk under element/
        let element = base.join("case-element").join("element");
        std::fs::create_dir_all(element.join("Config/Element")).unwrap();
        std::fs::write(element.join("Config/Element/Version.SW"), "1.8.4\r\n").unwrap();
        let (ver, src_loc) = release_version_with_source(&element).expect("found under element");
        assert_eq!(ver, "1.8.4");
        assert!(src_loc.starts_with("element:"), "{src_loc}");

        // 2. Case-insensitive component walk under element's parent/
        let parent_root = base.join("case-parent");
        let element_parent = parent_root.join("element");
        std::fs::create_dir_all(&element_parent).unwrap();
        std::fs::create_dir_all(parent_root.join("CONFIG/element")).unwrap();
        std::fs::write(parent_root.join("CONFIG/element/VERSION.sw"), "1.7.2\n").unwrap();
        let (ver, src_loc) =
            release_version_with_source(&element_parent).expect("found under parent");
        assert_eq!(ver, "1.7.2");
        assert!(src_loc.starts_with("parent:"), "{src_loc}");

        // 3. Inside configs.pck through the real pack reader
        let pck_element = base.join("case-pck").join("element");
        std::fs::create_dir_all(&pck_element).unwrap();
        std::fs::write(
            pck_element.join("configs.pck"),
            angelica_test_package(&[("config\\element\\version.sw", b"1.5.2\r\n")]),
        )
        .unwrap();
        let (ver, src_loc) =
            release_version_with_source(&pck_element).expect("found in configs.pck");
        assert_eq!(ver, "1.5.2");
        assert_eq!(src_loc, "configs.pck:config/element/version.sw");

        // 4. Folder with no marker anywhere
        let empty_element = base.join("case-none").join("element");
        std::fs::create_dir_all(&empty_element).unwrap();
        assert_eq!(release_version_with_source(&empty_element), None);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// `config\\element\\version.sw` is the client's own release marker, read as
    /// written rather than parsed into parts it does not promise.
    #[test]
    fn the_release_marker_is_read_as_written() {
        let root = std::env::temp_dir().join(format!("pw-version-sw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/element")).unwrap();
        let marker = root.join("config/element/version.sw");
        assert_eq!(
            release_version(&root),
            None,
            "absent marker is not reported"
        );
        std::fs::write(&marker, "1.8.4\r\n").unwrap();
        assert_eq!(release_version(&root).as_deref(), Some("1.8.4"));
        // A bare build number is the other format in circulation; it is
        // reported as itself rather than sliced into a version triple.
        std::fs::write(&marker, " 3300 \n").unwrap();
        assert_eq!(release_version(&root).as_deref(), Some("3300"));
        // Not text, or empty: no label from it.
        std::fs::write(&marker, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(release_version(&root), None);
        std::fs::write(&marker, "   \n").unwrap();
        assert_eq!(release_version(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_missing() {
        assert!(detect(Path::new("__not_a_client__")).is_err());
    }

    #[test]
    fn elements_header_reads_the_short_version_and_the_word() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x3000_001bu32.to_le_bytes());
        bytes.extend_from_slice(&0x4b39_c931u32.to_le_bytes());
        let header = ElementsHeader::parse(&bytes).unwrap();
        assert_eq!(header.short, 27);
        assert_eq!(header.version(), 27);
        assert_eq!(header.family(), 0x3000);
        assert_eq!(header.canonical(), 0x3000_001b);
        assert!(header.is_v27_layout());
        assert_eq!(header.describe(), "v27");
        assert_eq!(header.timestamp, 0x4b39_c931);
    }

    #[test]
    fn a_later_elements_version_is_reported_not_mistaken_for_v27() {
        // Other releases keep the family marker and change only the version
        // half: v29 and the much later v267 both read as themselves.
        for version in [29u32, 70, 85, 253, 267] {
            let word = (version & 0xffff) | 0x3000_0000;
            let mut bytes = word.to_le_bytes().to_vec();
            bytes.extend_from_slice(&0u32.to_le_bytes());
            let header = ElementsHeader::parse(&bytes).unwrap();
            assert_eq!(header.version(), version);
            assert_eq!(header.family(), 0x3000);
            assert_eq!(header.canonical(), word);
            assert!(!header.is_v27_layout());
        }
        // A different family with the same version number is also not v27.
        let mut foreign = 0x2000_001bu32.to_le_bytes().to_vec();
        foreign.extend_from_slice(&0u32.to_le_bytes());
        let header = ElementsHeader::parse(&foreign).unwrap();
        assert_eq!(header.version(), 27);
        assert!(!header.is_v27_layout());
    }

    #[test]
    fn short_files_do_not_panic() {
        assert!(ElementsHeader::parse(&[]).is_none());
        assert!(ElementsHeader::parse(&[0x1b]).is_none());
    }

    #[test]
    fn unix_timestamp_matches_the_client_tool() {
        // `gshop.data` header stamp from the 1.4.2 client (0x5ccc5f37).
        assert_eq!(format_unix_timestamp(0x5ccc_5f37), "2019-05-03 15:33:11");
        assert_eq!(format_unix_timestamp(0), "1970-01-01 00:00:00");
    }

    /// A `.pck` at `MAX_FILE_PACKAGE` is one half of a split archive. The scan
    /// has to say where the other half is: its directory reader is handed a
    /// single file, and "unsupported PCK version" or "bad package end offset"
    /// for a pack that is merely continued elsewhere is a fault the operator
    /// cannot act on.
    /// The version word is the last four bytes of the *trailer*, and the trailer
    /// ends where the prefix says it does. Reading from `size - 4` reports
    /// whatever happens to be last — for a split package's `.pck` half that is
    /// payload, and for a repacked archive it is trailing junk — as a version.
    #[test]
    fn the_container_version_comes_from_the_declared_trailer_not_the_file_end() {
        let root = std::env::temp_dir().join(format!("pw-ver-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        // A minimal 0x00020002 trailer with the version word in place…
        let mut pack = vec![0u8; 12];
        pack[0..4].copy_from_slice(&0x4dca23efu32.to_le_bytes());
        pack[8..12].copy_from_slice(&0x56a089b7u32.to_le_bytes());
        let mut trailer = vec![0u8; 280];
        trailer[0..4].copy_from_slice(&0xfdfdfeeeu32.to_le_bytes());
        trailer[268..272].copy_from_slice(&0xf00dbeefu32.to_le_bytes());
        trailer[276..280].copy_from_slice(&0x0002_0002u32.to_le_bytes());
        pack.extend_from_slice(&trailer);
        let declared = pack.len() as u32;
        pack[4..8].copy_from_slice(&declared.to_le_bytes());
        let path = root.join("exact.pck");
        fs::write(&path, &pack).unwrap();
        assert_eq!(
            container_version(&path, ContainerKind::Pck),
            Some(0x0002_0002)
        );

        // …and the same pack with four bytes of junk appended after it. The
        // prefix still declares the true end, so the version must not move.
        let mut padded = pack.clone();
        padded.extend_from_slice(&0xdead_beefu32.to_le_bytes());
        let path = root.join("padded.pck");
        fs::write(&path, &padded).unwrap();
        assert_eq!(
            container_version(&path, ContainerKind::Pck),
            Some(0x0002_0002),
            "trailing bytes must not become the reported version"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_split_pack_names_the_pkx_that_holds_its_other_half() {
        let root = std::env::temp_dir().join(format!("pw-split-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        // A prefix whose declared size is past the file, which is what the
        // first half of a split archive looks like.
        let mut pck = vec![0u8; 4096];
        pck[0..4].copy_from_slice(&0x4dca23efu32.to_le_bytes());
        pck[4..8].copy_from_slice(&0x7fff_ff00u32.to_le_bytes());
        pck[8..12].copy_from_slice(&0x56a089b7u32.to_le_bytes());
        fs::write(root.join("models.pck"), &pck).unwrap();
        assert_eq!(
            split_continuation(&root, "models.pck"),
            None,
            "a pack with no .pkx beside it has no continuation"
        );
        fs::write(root.join("models.pkx"), []).unwrap();
        assert_eq!(
            split_continuation(&root, "models.pck"),
            Some(root.join("models.pkx"))
        );
        // A small pack never has one, whatever sits next to it.
        fs::write(root.join("gfx.pck"), &pck[..2048]).unwrap();
        assert_eq!(split_continuation(&root, "gfx.pck"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn readable_map_dir_accepts_instance_files_and_case_insensitive_names() {
        let root =
            std::env::temp_dir().join(format!("pw-client-map-detect-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let a01 = root.join("maps/a01");
        fs::create_dir_all(&a01).unwrap();
        for ext in ["trn2", "ecwld"] {
            fs::write(a01.join(format!("a01.{ext}")), []).unwrap();
        }
        fs::write(a01.join("A01.ECBSD"), []).unwrap();
        assert_eq!(readable_map_dir(&root), Some(a01));
        fs::remove_dir_all(root).unwrap();
    }

    /// Build a tiny client tree: a readable world, the three required packs, a
    /// PE32 executable, an `elements.data` header and a `tasks.data` header.
    /// A multi-entry Angelica package: `12`-byte prefix, the payloads back to
    /// back, then one 8-byte prefix and zlib stream per 276-byte record, then
    /// the 280-byte trailer. Stored record order is entry order, because the
    /// reader walks the run backwards from the trailer and then reverses it.
    fn angelica_test_package(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::write::ZlibEncoder;
        use std::io::Write;
        let mut out = Vec::new();
        out.extend_from_slice(&0x4dca23efu32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // patched below
        out.extend_from_slice(&0x56a089b7u32.to_le_bytes());
        let mut offsets = Vec::new();
        for (_, payload) in entries {
            offsets.push(out.len() as u32);
            out.extend_from_slice(payload);
        }
        for (i, (name, payload)) in entries.iter().enumerate() {
            let mut record = vec![0u8; 276];
            record[..name.len()].copy_from_slice(name.as_bytes());
            record[260..264].copy_from_slice(&offsets[i].to_le_bytes());
            record[264..268].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            record[268..272].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            let mut encoder = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&record).unwrap();
            let stream = encoder.finish().unwrap();
            out.extend_from_slice(&[0u8; 8]); // record prefix
            out.extend_from_slice(&stream);
        }
        let mut header = vec![0u8; 280];
        header[0..4].copy_from_slice(&0xa508bdc4u32.to_le_bytes());
        header[4..8].copy_from_slice(&0x20002u32.to_le_bytes());
        header[268..272].copy_from_slice(&0x853567c8u32.to_le_bytes());
        header[272..276].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        header[276..280].copy_from_slice(&0x20002u32.to_le_bytes());
        out.extend_from_slice(&header);
        let total = out.len() as u32;
        out[4..8].copy_from_slice(&total.to_le_bytes());
        out
    }

    fn synthetic_client(tag: &str, elements_version: u16, tasks_version: u32) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pw-scan-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let element = root.join("element");
        let map = element.join("maps/world");
        fs::create_dir_all(&map).unwrap();
        for ext in ["trn2", "ecwld", "ecbsd"] {
            fs::write(map.join(format!("world.{ext}")), []).unwrap();
        }
        for name in REQUIRED_ARCHIVES {
            fs::write(element.join(name), []).unwrap();
        }
        // A minimal PE32 i386 header: the scanner only reads the machine word
        // and the resource directory offset, both of which are zero here.
        let mut pe = vec![0u8; 512];
        pe[0..2].copy_from_slice(b"MZ");
        let pe_offset = 0x40usize;
        pe[0x3c..0x40].copy_from_slice(&(pe_offset as u32).to_le_bytes());
        pe[pe_offset..pe_offset + 4].copy_from_slice(b"PE\0\0");
        pe[pe_offset + 4..pe_offset + 6].copy_from_slice(&0x014cu16.to_le_bytes());
        fs::write(element.join("elementclient.exe"), &pe).unwrap();
        fs::create_dir_all(element.join("data")).unwrap();
        // Same layout the real files use: family in the high half, version in
        // the low half (`0x3000_001b` for v27).
        let word = 0x3000_0000u32 | elements_version as u32;
        let mut elements = word.to_le_bytes().to_vec();
        elements.extend_from_slice(&0x4b39_c931u32.to_le_bytes());
        fs::write(element.join("data/elements.data"), elements).unwrap();
        let mut tasks = 0x9385_8361u32.to_le_bytes().to_vec();
        tasks.extend_from_slice(&tasks_version.to_le_bytes());
        fs::write(element.join("data/tasks.data"), tasks).unwrap();
        root
    }

    #[test]
    fn scan_reports_data_versions_independently_of_the_executable() {
        let root = synthetic_client("v27", 27, 75);
        let scan = scan(&root);
        assert!(scan.loadable(), "{}", scan.report());
        assert_eq!(scan.element, root.join("element"));
        assert!(scan.readable_map.is_some());
        let elements = scan.elements.as_ref().expect("elements.data");
        assert_eq!(elements.raw, Some(0x3000_001b));
        assert_eq!(elements.version.as_deref(), Some("v27"));
        let tasks = scan
            .data_files
            .iter()
            .find(|info| info.name == "tasks.data")
            .expect("tasks.data");
        assert_eq!(tasks.version.as_deref(), Some("v75"));
        // The synthetic executable is not the inspected build, so it is
        // reported as unverified rather than refused.
        let exe = scan.executable.as_ref().expect("executable");
        assert!(!exe.is_known());
        assert!(exe.label().starts_with("unverified build"));
        // The pair is the data version, and the headline carries both halves:
        // a 1.4.2 client can sit beside v27/v75 or v29/v78.
        assert_eq!(
            scan.data_versions(),
            (Some("v27".into()), Some("v75".into()))
        );
        assert_eq!(
            scan.data_version_summary(),
            "elements.data v27 / tasks.data v75"
        );
        assert!(scan.headline().contains("v27"));
        assert!(scan.headline().contains("v75"));
        // The card carries exactly the three requested versions, in order.
        let rows = scan.summary_rows();
        assert_eq!(
            rows.iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            vec!["game", "elements", "tasks"]
        );
        assert_eq!(rows[1].1, "v27");
        assert_eq!(rows[2].1, "v75");
        // Nothing else may share the card; the archive tally stays in the report.
        assert!(!rows.iter().any(|(_, value)| value.contains("archives")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_report_states_the_pair_before_the_per_file_lines() {
        let root = synthetic_client("report-pair", 27, 75);
        let scan = scan(&root);
        let report = scan.report();
        let pair = report
            .find("pair      elements.data v27 / tasks.data v75")
            .unwrap_or_else(|| panic!("pair line missing from:\n{report}"));
        let elements = report.find("elements.data    v27").expect("elements line");
        let tasks = report.find("tasks.data       v75").expect("tasks line");
        assert!(pair < elements && elements < tasks, "{report}");
        assert!(report.contains("move independently of the executable"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_tasks_file_leaves_the_elements_half_of_the_pair() {
        let root = synthetic_client("no-tasks", 27, 75);
        fs::remove_file(root.join("element/data/tasks.data")).unwrap();
        let scan = scan(&root);
        assert!(scan.loadable(), "{}", scan.report());
        assert_eq!(
            scan.data_versions(),
            (Some("v27".into()), None)
        );
        assert!(scan.data_version_summary().contains("tasks.data absent"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_client_on_another_data_revision_is_still_loadable() {
        // The same executable fingerprint with the RUS 1.4.2 data revision
        // beside it: reported as v29/v78, not refused.
        let root = synthetic_client("v29-pair", 29, 78);
        let scan = scan(&root);
        assert!(scan.loadable(), "{}", scan.report());
        assert_eq!(
            scan.data_versions(),
            (Some("v29".into()), Some("v78".into()))
        );
        assert_eq!(
            scan.data_version_summary(),
            "elements.data v29 / tasks.data v78"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scan_names_a_different_elements_version_instead_of_parsing_it_as_v27() {
        let root = synthetic_client("v29", 29, 78);
        let scan = scan(&root);
        assert!(scan.loadable(), "{}", scan.report());
        let elements = scan.elements.as_ref().expect("elements.data");
        assert_eq!(elements.raw, Some(0x3000_001d));
        assert_eq!(elements.version.as_deref(), Some("v29"));
        assert!(
            scan.notes
                .iter()
                .any(|note| note.contains("v29") && note.contains("v27 layout")),
            "{:?}",
            scan.notes
        );
        let tasks = scan
            .data_files
            .iter()
            .find(|info| info.name == "tasks.data")
            .expect("tasks.data");
        assert_eq!(tasks.version.as_deref(), Some("v78"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_broken_archive_is_named_in_the_report() {
        let root = synthetic_client("broken-pack", 27, 75);
        // `configs.pck` is optional, so a truncated one must be reported and
        // must not make the client unloadable.
        fs::write(root.join("element/configs.pck"), b"not a pack at all").unwrap();
        let scan = scan(&root);
        assert!(scan.loadable(), "{}", scan.report());
        let configs = scan
            .archives
            .iter()
            .find(|archive| archive.name == "configs.pck")
            .expect("configs.pck row");
        assert!(configs.problem.is_some());
        assert!(scan.report().contains("configs.pck"));
        // A broken optional archive is named in the report, not on the card.
        assert!(!scan
            .summary_rows()
            .iter()
            .any(|(_, value)| value.contains("configs.pck")));
        fs::remove_dir_all(root).unwrap();
    }

    /// `script.pck` is not a required pack, but a client that has one should
    /// see it in the report: the module opens "every `*.pck` beside the
    /// executable", and the 1.5.2 client ships this one next to `configs.pck`.
    #[test]
    fn the_script_pack_is_reported_when_the_client_ships_one() {
        let root = synthetic_client("script-pack", 27, 75);
        let element = root.join("element");
        // A two-entry Angelica pack, the shape the shipped one has.
        fs::write(
            element.join("script.pck"),
            angelica_test_package(&[("dir", b""), ("version.sw", b"0.525.0\r\n")]),
        )
        .unwrap();
        let scan = scan(&root);
        assert!(scan.loadable(), "{}", scan.report());
        let script = scan
            .archives
            .iter()
            .find(|archive| archive.name == "script.pck")
            .expect("script.pck row");
        assert_eq!(script.kind, ContainerKind::Angelica);
        assert_eq!(script.entries, Some(2));
        assert!(scan.report().contains("script.pck"), "{}", scan.report());
        fs::remove_dir_all(root).unwrap();
    }

    /// The shipped 1.5.2 `script.pck` holds a bare `version.sw`, not the
    /// `<stem>\version.sw` a patcher writes. Matching only the prefixed name
    /// meant the one real marker staged here was never read or printed.
    #[test]
    fn a_bare_version_sw_marker_is_read_like_a_prefixed_one() {
        let root = synthetic_client("version-sw", 27, 75);
        let element = root.join("element");
        fs::write(
            element.join("script.pck"),
            angelica_test_package(&[("dir", b""), ("version.sw", b"0.525.0\r\n")]),
        )
        .unwrap();
        let scan = scan(&root);
        let script = scan
            .archives
            .iter()
            .find(|archive| archive.name == "script.pck")
            .expect("script.pck row");
        assert_eq!(script.pack_version.as_deref(), Some("0.525.0"));
        assert!(
            scan.report().contains("version.sw 0.525.0"),
            "{}",
            scan.report()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_required_archive_fails_the_scan() {
        let root = synthetic_client("missing-pack", 27, 75);
        fs::remove_file(root.join("element/textures.pck")).unwrap();
        let scan = scan(&root);
        assert!(!scan.loadable());
        assert!(scan
            .errors
            .iter()
            .any(|error| error.contains("textures.pck")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn gshop_rows_print_their_header_timestamp() {
        let mut info = DataFileInfo {
            name: "gshop.data".into(),
            path: None,
            header: None,
            raw: None,
            version: None,
            detail: None,
            problem: None,
        };
        let mut bytes = 0x5ccc_5f37u32.to_le_bytes().to_vec();
        bytes.resize(64, 0);
        describe_data_file("gshop.data", &bytes, &mut info);
        assert_eq!(info.raw, Some(0x5ccc_5f37));
        assert!(info
            .detail
            .as_deref()
            .unwrap()
            .contains("2019-05-03 15:33:11"));
    }

    /// `task_npc.data` prints the record count at offset 10 but its guard only
    /// required the words at 0 and 8, which are satisfied by ten bytes. A file
    /// cut off at ten or eleven bytes therefore reported `0 records` -- a
    /// measurement it had not made, and one that reads as "this client ships no
    /// NPC tasks" rather than "this header is short".
    #[test]
    fn a_task_npc_header_missing_its_record_count_is_truncated() {
        // `describe_data_file` fills a struct rather than resetting it, so each
        // case starts from a fresh one the way `scan_data_files` does.
        fn describe(bytes: &[u8]) -> DataFileInfo {
            let mut info = DataFileInfo {
                name: "task_npc.data".into(),
                path: None,
                header: None,
                raw: None,
                version: None,
                detail: None,
                problem: None,
            };
            describe_data_file("task_npc.data", bytes, &mut info);
            info
        }
        let mut header = 4096u32.to_le_bytes().to_vec(); // declared size
        header.extend_from_slice(&0x0000_abcd_u32.to_le_bytes()); // timestamp at 4
        header.extend_from_slice(&9u16.to_le_bytes()); // version at 8

        for short in [header.clone(), [header.clone(), vec![0u8]].concat()] {
            assert!(short.len() < 12, "the count at 10 is what is missing");
            let info = describe(&short);
            assert_eq!(
                info.problem.as_deref(),
                Some("truncated header"),
                "{} bytes cannot carry the count the detail line prints",
                short.len()
            );
            assert_eq!(info.detail, None, "no detail is better than a wrong one");
        }

        // With both count bytes the same header is real and reports them.
        let mut whole = header.clone();
        whole.extend_from_slice(&42u16.to_le_bytes());
        let info = describe(&whole);
        assert_eq!(info.problem, None);
        let detail = info.detail.as_deref().unwrap();
        assert!(detail.contains("0x0000abcd"), "got {detail}");
        assert!(detail.contains("42 records"), "got {detail}");
    }

    /// `dyn_tasks.data` reads its timestamp at offset 4, which the guard at 8
    /// already guarantees, so this one could not report a fabricated zero --
    /// the test pins that rather than claiming a fix.
    #[test]
    fn a_dyn_tasks_header_is_guarded_past_its_timestamp() {
        for len in [0usize, 4, 8, 9] {
            let mut info = DataFileInfo {
                name: "dyn_tasks.data".into(),
                path: None,
                header: None,
                raw: None,
                version: None,
                detail: None,
                problem: None,
            };
            let mut bytes = vec![0u8; len];
            bytes[..len.min(4)].copy_from_slice(&[1, 2, 3, 4][..len.min(4)]);
            describe_data_file("dyn_tasks.data", &bytes, &mut info);
            assert_eq!(
                info.problem.as_deref(),
                Some("truncated header"),
                "{len} bytes cannot carry the version word at 8"
            );
        }
        let mut info = DataFileInfo {
            name: "dyn_tasks.data".into(),
            path: None,
            header: None,
            raw: None,
            version: None,
            detail: None,
            problem: None,
        };
        let mut bytes = 4096u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        bytes.extend_from_slice(&7u16.to_le_bytes());
        describe_data_file("dyn_tasks.data", &bytes, &mut info);
        assert_eq!(info.problem, None);
        assert!(info.detail.as_deref().unwrap().contains("0x12345678"));
    }

    /// A `VS_VERSIONINFO` blob with one `StringFileInfo / 040904b0 / String`
    /// pair per `(key, value)`. Built the way Windows builds it, including the
    /// `StringTable` value length being counted in UTF-16 units.
    fn synthetic_version_blob(entries: &[(&str, &str)]) -> Vec<u8> {
        synthetic_version_blob_with(entries, "040904b0")
    }

    /// The same builder, but with any `StringTable` key, so a test can author a
    /// localised table or several tables in one `StringFileInfo`.
    fn synthetic_version_blob_with(entries: &[(&str, &str)], table_key: &str) -> Vec<u8> {
        fn utf16(text: &str) -> Vec<u8> {
            version_utf16(text)
        }
        fn node(
            key: &str,
            value: &[u8],
            value_length: usize,
            kind: u16,
            children: &[u8],
        ) -> Vec<u8> {
            // Field offsets are relative to the node start, so the key pad
            // counts the 6-byte header. Aligning the key alone would put the
            // value two bytes early and shift every child with it.
            let mut body = utf16(key);
            while (6 + body.len()) % 4 != 0 {
                body.push(0);
            }
            body.extend_from_slice(value);
            body.extend_from_slice(children);
            let length = 6 + body.len();
            let mut out = Vec::with_capacity(length + 3);
            out.extend_from_slice(&(length as u16).to_le_bytes());
            out.extend_from_slice(&(value_length as u16).to_le_bytes());
            out.extend_from_slice(&kind.to_le_bytes());
            out.extend_from_slice(&body);
            // Each node starts on a DWORD boundary, so the builder pads the
            // node to four bytes even though `wLength` does not count the pad.
            while out.len() % 4 != 0 {
                out.push(0);
            }
            out
        }
        let mut strings = Vec::new();
        for (key, value) in entries {
            strings.extend_from_slice(&node(key, &utf16(value), value.len() + 1, 1, &[]));
        }
        let table = node(table_key, &[], 0, 1, &strings);
        let file_info = node("StringFileInfo", &[], 0, 1, &table);
        // Root value: VS_FIXEDFILEINFO (52 bytes), length in bytes.
        node("VS_VERSION_INFO", &[0u8; 52], 52, 0, &file_info)
    }

    /// Module-level copies of the builder's two primitives, for a test that has
    /// to assemble several `StringTable`s itself.
    fn version_utf16(text: &str) -> Vec<u8> {
        text.encode_utf16()
            .chain(Some(0))
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    fn version_node(
        key: &str,
        value: &[u8],
        value_length: usize,
        kind: u16,
        children: &[u8],
    ) -> Vec<u8> {
        let mut body = version_utf16(key);
        while (6 + body.len()) % 4 != 0 {
            body.push(0);
        }
        body.extend_from_slice(value);
        body.extend_from_slice(children);
        let length = 6 + body.len();
        let mut out = Vec::with_capacity(length + 3);
        out.extend_from_slice(&(length as u16).to_le_bytes());
        out.extend_from_slice(&(value_length as u16).to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&body);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out
    }

    #[test]
    fn version_resource_strings_are_read_from_the_string_tables() {
        let blob = synthetic_version_blob(&[
            ("FileVersion", "1.4.2.2305"),
            ("ProductVersion", "1.4.2.2305"),
            ("CompanyName", "Perfect World"),
        ]);
        assert_eq!(
            version_string(&blob, "FileVersion").as_deref(),
            Some("1.4.2.2305")
        );
        assert_eq!(
            version_string(&blob, "ProductVersion").as_deref(),
            Some("1.4.2.2305")
        );
        assert_eq!(
            version_string(&blob, "CompanyName").as_deref(),
            Some("Perfect World")
        );
        assert_eq!(version_string(&blob, "Comments"), None);
    }

    /// A resource authored in two languages: the first `StringTable` is the
    /// localised one and the English table follows it. Reading whichever table
    /// came first would report the localised product name and version, and a
    /// patch check compares exactly those two strings against a known build.
    #[test]
    fn the_english_string_table_wins_over_one_that_precedes_it() {
        fn table(language: &str, version: &str) -> Vec<u8> {
            let mut strings = Vec::new();
            for key in ["FileVersion", "ProductVersion"] {
                strings.extend_from_slice(&version_node(
                    key,
                    &version_utf16(version),
                    version.len() + 1,
                    1,
                    &[],
                ));
            }
            version_node(language, &[], 0, 1, &strings)
        }
        let localized = table("080404b0", "1.4.2.9999");
        let english = table("040904b0", "1.4.2.2305");
        let mut both = localized.clone();
        both.extend_from_slice(&english);
        let blob = version_node(
            "VS_VERSION_INFO",
            &[0u8; 52],
            52,
            0,
            &version_node("StringFileInfo", &[], 0, 1, &both),
        );
        assert_eq!(
            version_string(&blob, "FileVersion").as_deref(),
            Some("1.4.2.2305"),
            "the English table must win even though it comes second"
        );
        assert_eq!(
            version_string(&blob, "ProductVersion").as_deref(),
            Some("1.4.2.2305")
        );
        // With the English table removed the localised one is still readable:
        // a build with no English resource must not lose its version.
        let only_localized = version_node(
            "VS_VERSION_INFO",
            &[0u8; 52],
            52,
            0,
            &version_node("StringFileInfo", &[], 0, 1, &localized),
        );
        assert_eq!(
            version_string(&only_localized, "FileVersion").as_deref(),
            Some("1.4.2.9999")
        );
    }

    /// A minimal but structurally real PE32 image: one `.rsrc` section holding
    /// a three-level resource directory and a `VS_VERSIONINFO` blob, so the
    /// resource walk can be exercised without shipping a binary. The `PE` is
    /// deserialised by the real `goblin`, which is why the header has to be
    /// laid out properly rather than hand-built.
    fn pe_with_version_blob(blob: &[u8]) -> Vec<u8> {
        const FILE_ALIGN: usize = 0x200;
        const SECTION_RVA: usize = 0x1000;
        // Resource directory: root (RT_VERSION -> name -> language), then the
        // `IMAGE_RESOURCE_DATA_ENTRY` the leaf points at.
        let root = 0usize;
        let type_dir = 0x18;
        let name_dir = 0x30;
        let data_entry = 0x60;
        let blob_at = 0x70;
        let mut rsrc = vec![0u8; blob_at + blob.len()];
        fn put16(data: &mut [u8], at: usize, value: u16) {
            data[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        fn put32(data: &mut [u8], at: usize, value: u32) {
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        // Exactly three directory levels, as Windows writes them: type
        // (`RT_VERSION` = 16), name (the resource id), language. The language
        // level's entry has bit 31 clear and points at the data entry.
        for (at, named, ids, entry_name, target) in [
            (root, 0, 1, 16u32, 0x8000_0000u32 | type_dir as u32),
            (type_dir, 0, 1, 1, 0x8000_0000 | name_dir as u32),
            (name_dir, 0, 1, 0x0409, data_entry as u32),
        ] {
            put16(&mut rsrc, at + 12, named);
            put16(&mut rsrc, at + 14, ids);
            put32(&mut rsrc, at + 16, entry_name);
            put32(&mut rsrc, at + 20, target);
        }
        put32(&mut rsrc, data_entry, (SECTION_RVA + blob_at) as u32);
        put32(&mut rsrc, data_entry + 4, blob.len() as u32);
        rsrc[blob_at..].copy_from_slice(blob);
        let raw_size = rsrc.len().div_ceil(FILE_ALIGN) * FILE_ALIGN;

        PeBuilder::new(SECTION_RVA, rsrc, raw_size).finish()
    }

    /// Just enough of a PE32 writer for the test above.
    struct PeBuilder {
        rva: usize,
        data: Vec<u8>,
        raw_size: usize,
    }

    impl PeBuilder {
        fn new(rva: usize, data: Vec<u8>, raw_size: usize) -> Self {
            Self {
                rva,
                data,
                raw_size,
            }
        }

        fn finish(&self) -> Vec<u8> {
            const FILE_ALIGN: usize = 0x200;
            let pe_offset = 0x40usize;
            let optional_size = 0xe0usize;
            let header_end = pe_offset + 24 + optional_size + 40;
            fn put16(data: &mut [u8], at: usize, value: u16) {
                data[at..at + 2].copy_from_slice(&value.to_le_bytes());
            }
            fn put32(data: &mut [u8], at: usize, value: u32) {
                data[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            let mut image = vec![0u8; header_end + self.raw_size];
            image[0] = b'M';
            image[1] = b'Z';
            put32(&mut image, 0x3c, pe_offset as u32);
            image[pe_offset..pe_offset + 4].copy_from_slice(b"PE\0\0");
            put16(&mut image, pe_offset + 4, 0x14c); // i386
            put16(&mut image, pe_offset + 6, 1); // one section
            put16(&mut image, pe_offset + 20, optional_size as u16);
            let opt = pe_offset + 24;
            put16(&mut image, opt, 0x10b); // PE32
            put32(&mut image, opt + 28, 0x0040_0000); // ImageBase
            put32(&mut image, opt + 32, FILE_ALIGN as u32);
            put32(&mut image, opt + 36, FILE_ALIGN as u32);
            put32(&mut image, opt + 56, (header_end + self.raw_size) as u32);
            put32(&mut image, opt + 60, header_end as u32);
            put32(&mut image, opt + 92, 16); // NumberOfRvaAndSizes
                                             // DataDirectory[2] = resources.
            put32(&mut image, opt + 96 + 16, self.rva as u32);
            put32(&mut image, opt + 96 + 20, self.data.len() as u32);
            let section = opt + optional_size;
            image[section..section + 5].copy_from_slice(b".rsrc");
            put32(&mut image, section + 8, self.data.len() as u32);
            put32(&mut image, section + 12, self.rva as u32);
            put32(&mut image, section + 16, self.raw_size as u32);
            put32(&mut image, section + 20, header_end as u32);
            put32(&mut image, section + 36, 0x4000_0040); // read, initialised
            image[header_end..header_end + self.data.len()].copy_from_slice(&self.data);
            image
        }
    }

    #[test]
    fn the_version_resource_is_reached_through_the_resource_directory() {
        let blob = synthetic_version_blob(&[
            ("FileVersion", "1.4.2.2305"),
            ("ProductVersion", "1.4.2.2305"),
        ]);
        let image = pe_with_version_blob(&blob);
        let pe = goblin::pe::PE::parse(&image).unwrap();
        assert!(!pe.is_64);
        let found = version_blob(&image, &pe).expect("version resource");
        assert_eq!(
            found,
            &blob[..],
            "the walk must return the resource verbatim"
        );
        assert_eq!(
            version_string(found, "FileVersion").as_deref(),
            Some("1.4.2.2305")
        );
    }

    #[test]
    fn an_image_without_a_resource_directory_yields_no_version_strings() {
        let image = PeBuilder::new(0x1000, vec![0u8; 8], 0x200).finish();
        let pe = goblin::pe::PE::parse(&image).unwrap();
        assert_eq!(version_blob(&image, &pe), None);
        assert_eq!(version_string(b"", "FileVersion"), None);
    }

    #[test]
    fn a_version_blob_without_string_tables_yields_nothing_instead_of_noise() {
        let blob = synthetic_version_blob(&[]);
        assert_eq!(version_string(&blob, "FileVersion"), None);
        assert_eq!(version_string(&[], "FileVersion"), None);
        assert_eq!(version_string(b"not a resource", "FileVersion"), None);
    }

    #[test]
    fn known_rows_name_every_fingerprint_constant() {
        for build in KNOWN_BUILDS {
            assert!(!build.label.is_empty());
            assert_eq!(build.sha256.len(), 64);
            assert!(!build.data_versions.is_empty());
        }
    }
}

//! Music playlists and polygons from the retail client's UTF-16 precinct.clt.
use crate::pck;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

pub fn default_zones_path(element: &Path) -> PathBuf {
    element.join("maps/world/precinct.clt")
}
#[derive(Debug)]
pub struct Zone {
    pub name: String,
    /// Authored centre point present in version-7 instance precincts. It is not
    /// a teleport target, but is a useful safe fly-to location when maps ship
    /// no `world_targets.txt`.
    pub center: Option<[f32; 3]>,
    polygon: Vec<[f32; 2]>,
    pub tracks: Vec<String>,
    /// Precinct stereo ambience paths surrounding the playlist.
    pub ambience_day: String,
    pub ambience_night: String,
    area: f32,
}
pub struct Zones {
    pub zones: Vec<Zone>,
}
fn quoted(s: &str) -> Result<String, String> {
    let s = s.trim();
    if !s.starts_with('"') {
        return Err(format!("expected quoted precinct string: {s}"));
    }
    let end = s[1..].find('"').ok_or("unterminated precinct string")? + 1;
    Ok(s[1..end].to_string())
}
fn vector(s: &str) -> Result<[f32; 3], String> {
    let vals: Vec<_> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|x| !x.is_empty())
        .map(str::parse::<f32>)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    if vals.len() != 3 || vals.iter().any(|x| !x.is_finite()) {
        return Err(format!("invalid precinct vertex {s}"));
    }
    Ok([vals[0], vals[1], vals[2]])
}
impl Zones {
    pub fn open(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        if bytes.len() < 2 || bytes.len() % 2 != 0 {
            return Err("invalid UTF-16 precinct file".into());
        }
        let words: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|x| u16::from_le_bytes([x[0], x[1]]))
            .collect();
        let text = String::from_utf16(&words).map_err(|e| e.to_string())?;
        Self::parse(text.trim_start_matches('\u{feff}'))
    }
    pub fn parse(text: &str) -> Result<Self, String> {
        let lines: Vec<_> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            .collect();
        if lines.len() < 2 {
            return Err("truncated precinct file".into());
        }
        let header = lines[0].split_whitespace().collect::<Vec<_>>();
        if header == ["version", "7"] {
            return Self::parse_instance_zones(&lines[2..]);
        }
        if header != ["version", "6"] {
            return Err("unsupported precinct version (expected 6 or 7)".into());
        }
        let mut i = 2usize;
        let mut zones = Vec::new();
        while i < lines.len() {
            let name = quoted(lines[i])?;
            let nums: Vec<_> = lines
                .get(i + 1)
                .ok_or("missing precinct header")?
                .split_whitespace()
                .map(str::parse::<usize>)
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            if nums.len() < 10
                || nums[1] < 3
                || nums[1] > 10000
                || nums[2] > 10000
                || nums[5] > 1000
            {
                return Err(format!("invalid precinct counts at {name}"));
            }
            let (n, labels, tracks) = (nums[1], nums[2], nums[5]);
            let begin = i + 3;
            let stop = begin.checked_add(n).ok_or("overflow")?;
            if stop + labels + tracks + 2 > lines.len() {
                return Err(format!("truncated precinct {name}"));
            }
            let mut poly = Vec::with_capacity(n);
            for line in &lines[begin..stop] {
                let [x, _, z] = vector(line)?;
                poly.push([x, z]);
            }
            let mut area = 0.;
            for j in 0..n {
                let p = poly[j];
                let q = poly[(j + 1) % n];
                area += p[0] * q[1] - q[0] * p[1];
            }
            let list = stop + labels + 1;
            let ambience_day = quoted(lines[stop + labels])?;
            let ambience_night = quoted(lines[list + tracks])?;
            let mut playlist = Vec::new();
            for line in &lines[list..list + tracks] {
                let path = quoted(line)?;
                if !path.is_empty() {
                    playlist.push(path)
                }
            }
            zones.push(Zone {
                name,
                center: None,
                polygon: poly,
                tracks: playlist,
                ambience_day,
                ambience_night,
                area: area.abs() * 0.5,
            });
            i = list + tracks + 1;
        }
        Ok(Self { zones })
    }
    /// Version 7 `precinct.clt` files ship with instance maps (`a01`, ...).
    /// They keep the zone name, a numeric header, one centre line, a polygon,
    /// and two empty sound paths; no playlist tracks or stereo ambience.
    fn parse_instance_zones(lines: &[&str]) -> Result<Self, String> {
        fn header(line: &str) -> Option<Vec<usize>> {
            line.split_whitespace()
                .map(str::parse::<usize>)
                .collect::<Result<Vec<_>, _>>()
                .ok()
                .filter(|nums| nums.len() >= 10)
        }
        let mut zones = Vec::new();
        let mut i = 0usize;
        while i + 1 < lines.len() {
            let Ok(name) = quoted(lines[i]) else {
                i += 1;
                continue;
            };
            let Some(nums) = header(lines[i + 1]) else {
                i += 1;
                continue;
            };
            let n = nums[1];
            if !(3..=10000).contains(&n) {
                return Err(format!("invalid precinct counts at {name}"));
            }
            // The first vector is the zone centre; the following `n` vectors are
            // its polygon. Some instance files repeat the first vertex at the end.
            let center = vector(lines.get(i + 2).ok_or("missing precinct centre")?)?;
            let begin = i + 3;
            let stop = begin.checked_add(n).ok_or("overflow")?;
            if stop > lines.len() {
                return Err(format!("truncated precinct {name}"));
            }
            let mut poly = Vec::with_capacity(n);
            for line in &lines[begin..stop] {
                let [x, _, z] = vector(line)?;
                poly.push([x, z]);
            }
            let mut area = 0.;
            for j in 0..n {
                let p = poly[j];
                let q = poly[(j + 1) % n];
                area += p[0] * q[1] - q[0] * p[1];
            }
            zones.push(Zone {
                name,
                center: Some(center),
                polygon: poly,
                tracks: Vec::new(),
                ambience_day: String::new(),
                ambience_night: String::new(),
                area: area.abs() * 0.5,
            });
            i = stop;
            // Skip the two authored sound-path strings and any blank separators;
            // the next record is a name followed by a numeric header.
            while i + 1 < lines.len() && header(lines[i + 1]).is_none() {
                i += 1;
            }
        }
        if zones.is_empty() {
            return Err("no instance precinct zones".into());
        }
        Ok(Self { zones })
    }
    pub fn find(&self, x: f32, z: f32) -> Option<usize> {
        self.zones
            .iter()
            .enumerate()
            .filter(|(_, p)| contains(&p.polygon, x, z))
            .min_by(|(_, a), (_, b)| a.area.total_cmp(&b.area))
            .map(|(i, _)| i)
    }
}
fn contains(poly: &[[f32; 2]], x: f32, z: f32) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let [xi, zi] = poly[i];
        let [xj, zj] = poly[j];
        if (zi > z) != (zj > z) && x < (xj - xi) * (z - zi) / (zj - zi) + xi {
            inside = !inside
        }
        j = i;
    }
    inside
}
#[derive(Default)]
struct DirectoryIndex {
    // Cache each directory's case-insensitive names once. Zone changes then
    // never re-enumerate the same music folder on the render/update thread.
    directories: HashMap<PathBuf, Option<HashMap<String, PathBuf>>>,
    resolutions: HashMap<String, Option<PathBuf>>,
}
impl DirectoryIndex {
    fn resolve_under(&mut self, root: &Path, raw: &str, prefix: &str) -> Option<PathBuf> {
        let cache_key = format!(
            "{}|{}",
            prefix.to_ascii_lowercase(),
            raw.to_ascii_lowercase()
        );
        if let Some(cached) = self.resolutions.get(&cache_key) {
            return cached.clone();
        }
        let parts: Vec<_> = raw.split(['\\', '/']).filter(|s| !s.is_empty()).collect();
        let result = if parts.is_empty()
            || !parts[0].eq_ignore_ascii_case(prefix)
            || parts.iter().any(|p| *p == ".." || *p == ".")
        {
            None
        } else {
            let mut path = root.to_path_buf();
            let mut found = true;
            for part in parts {
                if !self.directories.contains_key(&path) {
                    let entries = fs::read_dir(&path).ok().map(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .map(|entry| {
                                (
                                    entry.file_name().to_string_lossy().to_lowercase(),
                                    entry.path(),
                                )
                            })
                            .collect::<HashMap<_, _>>()
                    });
                    self.directories.insert(path.clone(), entries);
                }
                match self
                    .directories
                    .get(&path)
                    .and_then(Option::as_ref)
                    .and_then(|entries| entries.get(&part.to_lowercase()))
                {
                    Some(next) => path = next.clone(),
                    None => {
                        found = false;
                        break;
                    }
                }
            }
            if found && path.is_file() {
                Some(path)
            } else {
                None
            }
        };
        self.resolutions.insert(cache_key, result.clone());
        result
    }
}
pub(crate) fn resolve_under(element: &Path, raw: &str, prefix: &str) -> Option<PathBuf> {
    DirectoryIndex::default().resolve_under(element, raw, prefix)
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum Track {
    Loose(PathBuf),
    Packed { archive: PathBuf, entry: pck::Entry },
}
impl Track {
    fn label(&self) -> String {
        match self {
            Track::Loose(p) => p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            Track::Packed { entry, .. } => format!(
                "{} (sfx.pck)",
                entry.name.rsplit('/').next().unwrap_or(&entry.name)
            ),
        }
    }
    #[cfg(windows)]
    fn read(&self) -> Result<Vec<u8>, String> {
        match self {
            Track::Loose(p) => fs::read(p).map_err(|e| e.to_string()),
            Track::Packed { archive, entry } => {
                let mut f = fs::File::open(archive).map_err(|e| e.to_string())?;
                let r = pck::read_entry(&mut f, entry);
                r
            }
        }
    }
}
fn packed_key(raw: &str) -> Option<String> {
    let path = raw.replace('\\', "/");
    let (prefix, name) = path.split_once('/')?;
    if !prefix.eq_ignore_ascii_case("music")
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || part.contains(':'))
    {
        return None;
    }
    Some(format!("sfx/music/{}", name.to_lowercase()))
}
fn catalog(element: &Path) -> Result<HashMap<String, pck::Entry>, String> {
    let mut file = fs::File::open(element.join("sfx.pck")).map_err(|e| e.to_string())?;
    Ok(pck::directory(&mut file)?
        .into_iter()
        .filter(|e| e.name.to_ascii_lowercase().starts_with("sfx/music/"))
        .map(|e| (e.name.to_lowercase(), e))
        .collect())
}
fn resolve_track_indexed(
    element: &Path,
    packed: &HashMap<String, pck::Entry>,
    raw: &str,
    loose: &mut DirectoryIndex,
) -> Option<Track> {
    if let Some(path) = loose.resolve_under(element, raw, "music") {
        return Some(Track::Loose(path));
    }
    // Client playlists may name MP3 while a distribution ships loose OGG.
    if raw.to_ascii_lowercase().ends_with(".mp3") {
        let alt = format!("{}.ogg", &raw[..raw.len() - 4]);
        if let Some(path) = loose.resolve_under(element, &alt, "music") {
            return Some(Track::Loose(path));
        }
    }
    let key = packed_key(raw)?;
    let alternate = key.strip_suffix(".mp3").map(|stem| format!("{stem}.ogg"));
    packed
        .get(&key)
        .or_else(|| alternate.as_ref().and_then(|key| packed.get(key)))
        .cloned()
        .map(|entry| Track::Packed {
            archive: element.join("sfx.pck"),
            entry,
        })
}
#[derive(Debug)]
struct Request {
    enabled: bool,
    volume: f32,
    repeat: bool,
    zone: Option<usize>,
    paths: Vec<Track>,
}
pub struct MusicPlayer {
    zones: Option<Zones>,
    element: PathBuf,
    packed: HashMap<String, pck::Entry>,
    loose_index: DirectoryIndex,
    zone_error: Option<String>,
    catalog_error: Option<String>,
    unresolved: Vec<String>,
    last_zone: Option<usize>,
    last_enabled: bool,
    last_volume: f32,
    last_check: Instant,
    status: String,
    #[cfg(windows)]
    tx: std::sync::mpsc::Sender<Request>,
    #[cfg(windows)]
    rx: std::sync::mpsc::Receiver<String>,
}
impl MusicPlayer {
    pub fn new(element: &Path) -> Self {
        Self::new_for_map(element, &default_zones_path(element))
    }
    /// Use the selected map's precinct zones rather than the main world's.
    pub fn new_for_map(element: &Path, zones_path: &Path) -> Self {
        let zones = Zones::open(zones_path);
        let status = match &zones {
            Ok(z) => format!("{} music zones loaded", z.zones.len()),
            Err(e) => format!("Music zones unavailable: {e}"),
        };
        let zone_error = zones.as_ref().err().cloned();
        let (packed, catalog_error) = match catalog(element) {
            Ok(packed) => (packed, None),
            Err(error) => (HashMap::new(), Some(error)),
        };
        #[cfg(windows)]
        let (tx, rx) = {
            let (tx, request_rx) = std::sync::mpsc::channel();
            let (status_tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("zone-music".into())
                .spawn(move || audio_worker(request_rx, status_tx))
                .expect("music worker");
            (tx, rx)
        };
        Self {
            zones: zones.ok(),
            element: element.to_path_buf(),
            packed,
            loose_index: DirectoryIndex::default(),
            zone_error,
            catalog_error,
            unresolved: Vec::new(),
            last_zone: None,
            last_enabled: false,
            last_volume: -1.,
            last_check: Instant::now() - std::time::Duration::from_secs(2),
            status,
            #[cfg(windows)]
            tx,
            #[cfg(windows)]
            rx,
        }
    }
    pub fn update(&mut self, enabled: bool, volume: f32, x: f32, z: f32) {
        #[cfg(windows)]
        while let Ok(s) = self.rx.try_recv() {
            if enabled {
                self.status = s;
            }
        }
        if self.last_check.elapsed() < std::time::Duration::from_millis(100)
            && enabled == self.last_enabled
            && (volume - self.last_volume).abs() < 0.001
        {
            return;
        }
        self.last_check = Instant::now();
        let zone = if enabled {
            self.zones.as_ref().and_then(|db| db.find(x, z))
        } else {
            None
        };
        if zone == self.last_zone
            && enabled == self.last_enabled
            && (volume - self.last_volume).abs() < 0.001
        {
            return;
        }
        self.last_zone = zone;
        self.last_enabled = enabled;
        self.last_volume = volume;
        let mut paths = Vec::new();
        self.unresolved.clear();
        if let Some(q) = zone.and_then(|i| self.zones.as_ref().map(|z| &z.zones[i])) {
            for path in &q.tracks {
                match resolve_track_indexed(
                    &self.element,
                    &self.packed,
                    path,
                    &mut self.loose_index,
                ) {
                    Some(track) => paths.push(track),
                    None => self.unresolved.push(path.clone()),
                }
            }
        }
        let label = zone
            .and_then(|i| self.zones.as_ref().map(|z| z.zones[i].name.as_str()))
            .unwrap_or("No music zone");
        self.status = if !enabled {
            "Music off".into()
        } else if paths.is_empty() {
            format!("{label}: no available tracks in client")
        } else {
            format!("{label}: {} available tracks", paths.len())
        };
        let req = Request {
            enabled,
            volume,
            repeat: false,
            zone,
            paths,
        };
        #[cfg(windows)]
        let _ = self.tx.send(req);
        #[cfg(not(windows))]
        {
            let _ = req;
            if enabled {
                self.status.push_str(" (audio playback is Windows-only)");
            }
        }
    }
    pub fn status(&self) -> &str {
        &self.status
    }
    /// Keep lookup errors visible even while the playback worker reports a track.
    pub fn diagnostic(&self) -> String {
        let mut messages = Vec::new();
        if let Some(error) = &self.zone_error {
            messages.push(format!("Music zones unavailable: {error}"));
        }
        if let Some(error) = &self.catalog_error {
            messages.push(format!("sfx.pck unavailable: {error}"));
        }
        if !self.unresolved.is_empty() {
            let examples = self
                .unresolved
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            messages.push(format!(
                "{} unresolved playlist track(s): {examples}{}",
                self.unresolved.len(),
                if self.unresolved.len() > 5 {
                    ", …"
                } else {
                    ""
                }
            ));
        }
        messages.join("; ")
    }
}
/// Stereo precinct ambience. Selection and day/night boundary are viewer
/// approximations, not a reconstruction of the native transitions or 3D SFX.
pub struct AmbiencePlayer {
    zones: Option<Zones>,
    zone_error: Option<String>,
    element: PathBuf,
    loose_index: DirectoryIndex,
    last_zone: Option<usize>,
    last_night: bool,
    last_enabled: bool,
    last_volume: f32,
    last_check: Instant,
    status: String,
    #[cfg(windows)]
    tx: std::sync::mpsc::Sender<Request>,
    #[cfg(windows)]
    rx: std::sync::mpsc::Receiver<String>,
}
impl AmbiencePlayer {
    pub fn new(element: &Path) -> Self {
        Self::new_for_map(element, &default_zones_path(element))
    }
    /// Use the selected map's precinct zones rather than the main world's.
    pub fn new_for_map(element: &Path, zones_path: &Path) -> Self {
        let zones = Zones::open(zones_path);
        let status = match &zones {
            Ok(z) => format!("{} ambience zones loaded", z.zones.len()),
            Err(e) => format!("Ambience zones unavailable: {e}"),
        };
        let zone_error = zones.as_ref().err().cloned();
        #[cfg(windows)]
        let (tx, rx) = {
            let (tx, request_rx) = std::sync::mpsc::channel();
            let (status_tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("zone-ambience".into())
                .spawn(move || audio_worker(request_rx, status_tx))
                .expect("ambience worker");
            (tx, rx)
        };
        Self {
            zones: zones.ok(),
            zone_error,
            element: element.to_path_buf(),
            loose_index: DirectoryIndex::default(),
            last_zone: None,
            last_night: false,
            last_enabled: false,
            last_volume: -1.,
            last_check: Instant::now() - std::time::Duration::from_secs(2),
            status,
            #[cfg(windows)]
            tx,
            #[cfg(windows)]
            rx,
        }
    }
    pub fn update(&mut self, enabled: bool, volume: f32, x: f32, z: f32, night: bool) {
        #[cfg(windows)]
        while let Ok(s) = self.rx.try_recv() {
            if enabled {
                self.status = s;
            }
        }
        if self.last_check.elapsed() < std::time::Duration::from_millis(100)
            && enabled == self.last_enabled
            && night == self.last_night
            && (volume - self.last_volume).abs() < 0.001
        {
            return;
        }
        self.last_check = Instant::now();
        let zone = if enabled {
            self.zones.as_ref().and_then(|db| db.find(x, z))
        } else {
            None
        };
        if zone == self.last_zone
            && night == self.last_night
            && enabled == self.last_enabled
            && (volume - self.last_volume).abs() < 0.001
        {
            return;
        }
        self.last_zone = zone;
        self.last_night = night;
        self.last_enabled = enabled;
        self.last_volume = volume;
        let selected = zone.and_then(|i| self.zones.as_ref().map(|db| &db.zones[i]));
        let raw = selected.map(|q| {
            if night {
                &q.ambience_night
            } else {
                &q.ambience_day
            }
        });
        let path = raw.and_then(|p| {
            self.loose_index
                .resolve_under(&self.element.join("music"), p, "ambiencestereo")
        });
        let paths = path.map(Track::Loose).into_iter().collect::<Vec<_>>();
        self.status = if !enabled {
            "Ambience off".into()
        } else if let Some(error) = &self.zone_error {
            format!("Ambience zones unavailable: {error}")
        } else if paths.is_empty() {
            format!(
                "{}: ambience unavailable ({})",
                selected.map(|z| z.name.as_str()).unwrap_or("No zone"),
                raw.map(String::as_str).unwrap_or("no path")
            )
        } else {
            format!(
                "{}: {}",
                selected.map(|z| z.name.as_str()).unwrap_or("Zone"),
                paths[0].label()
            )
        };
        let req = Request {
            enabled,
            volume,
            repeat: true,
            zone,
            paths,
        };
        #[cfg(windows)]
        let _ = self.tx.send(req);
        #[cfg(not(windows))]
        {
            let _ = req;
            if enabled {
                self.status.push_str(" (audio playback is Windows-only)");
            }
        }
    }
    pub fn status(&self) -> &str {
        &self.status
    }
}
#[cfg(windows)]
fn audio_worker(rx: std::sync::mpsc::Receiver<Request>, status: std::sync::mpsc::Sender<String>) {
    use rodio::{Decoder, OutputStream, Sink, Source};
    use std::{
        io::{BufReader, Cursor},
        time::Duration,
    };
    let mut output = None;
    let mut sink: Option<Sink> = None;
    let mut current = None;
    let mut paths = Vec::new();
    let mut index = 0usize;
    let mut volume = 0.5f32;
    let mut enabled = false;
    let mut repeat = false;
    let mut retry_at = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(mut req) => {
                // Rapid flight can cross several precincts while a track is
                // being opened/decoded. Collapse queued selections to the
                // newest location so we never play a stale intermediate zone.
                while let Ok(newer) = rx.try_recv() {
                    req = newer;
                }
                let changed = req.zone != current
                    || req.enabled != enabled
                    || req.paths != paths
                    || req.repeat != repeat;
                volume = req.volume.clamp(0., 1.);
                enabled = req.enabled;
                repeat = req.repeat;
                if changed {
                    if let Some(s) = sink.take() {
                        s.stop();
                    }
                    paths = req.paths;
                    current = req.zone;
                    index = 0;
                    retry_at = Instant::now();
                }
                if let Some(s) = &sink {
                    s.set_volume(volume)
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if !enabled || paths.is_empty() || Instant::now() < retry_at {
            continue;
        }
        if output.is_none() {
            match OutputStream::try_default() {
                Ok(o) => output = Some(o),
                Err(e) => {
                    let _ = status.send(format!("Audio output unavailable: {e}"));
                    retry_at = Instant::now() + Duration::from_secs(2);
                    continue;
                }
            }
        }
        if sink.as_ref().map_or(true, |s| s.empty()) {
            let path = &paths[index % paths.len()];
            index += 1;
            let track = path.read().and_then(|bytes| {
                Decoder::new(BufReader::new(Cursor::new(bytes))).map_err(|e| e.to_string())
            });
            let mut latest = None;
            while let Ok(req) = rx.try_recv() {
                latest = Some(req);
            }
            if let Some(req) = latest {
                let changed = req.zone != current
                    || req.enabled != enabled
                    || req.paths != paths
                    || req.repeat != repeat;
                volume = req.volume.clamp(0., 1.);
                enabled = req.enabled;
                repeat = req.repeat;
                if changed {
                    if let Some(s) = sink.take() {
                        s.stop();
                    }
                    paths = req.paths;
                    current = req.zone;
                    index = 0;
                    retry_at = Instant::now();
                    continue;
                }
            }
            match track {
                Ok(source) => {
                    let s = match Sink::try_new(&output.as_ref().unwrap().1) {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = status.send(format!("Audio output error: {e}"));
                            retry_at = Instant::now() + Duration::from_secs(2);
                            continue;
                        }
                    };
                    s.set_volume(volume);
                    if repeat {
                        s.append(source.repeat_infinite());
                    } else {
                        s.append(source);
                    }
                    let _ = status.send(format!("Playing: {}", path.label()));
                    sink = Some(s);
                }
                Err(e) => {
                    let _ = status.send(format!("Cannot decode {}: {e}", path.label()));
                    retry_at = Instant::now() + Duration::from_secs(1);
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packed_paths_preserve_format_and_reject_traversal() {
        assert_eq!(
            packed_key("Music\\island2_n.mp3").as_deref(),
            Some("sfx/music/island2_n.mp3")
        );
        assert_eq!(
            packed_key("MUSIC/city_d1.OGG").as_deref(),
            Some("sfx/music/city_d1.ogg")
        );
        assert!(packed_key("Music/../../secret.mp3").is_none());
    }
    #[test]
    fn parse_regions_and_select_smallest() {
        let s="version 6\n123\n\"large\"\n1 5 0 0 1 1 60 1 1 0\n0, 0, 0\n-10 0 -10\n10 0 -10\n10 0 10\n-10 0 10\n-10 0 -10\n\"\"\n\"Music\\a.mp3\"\n\"\"\n\"small\"\n2 5 0 0 1 1 60 1 1 0\n0, 0, 0\n-1 0 -1\n1 0 -1\n1 0 1\n-1 0 1\n-1 0 -1\n\"\"\n\"Music\\b.ogg\"\n\"\"";
        let z = Zones::parse(s).unwrap();
        assert_eq!(z.find(0., 0.), Some(1));
        assert_eq!(z.find(5., 5.), Some(0));
        assert_eq!(z.find(50., 50.), None);
        assert_eq!(z.zones[1].tracks[0], "Music\\b.ogg");
        assert_eq!(z.zones[1].ambience_day, "");
    }
    #[test]
    fn instance_precinct_version_7_keeps_zone_names_and_polygons() {
        let text = r#"version 7
1220409703
"Mirror"
101001 5 0 0 101 0 60 1 101 0 0
-455.420013, 18.000000, 420.273987
-505.866333 100.492149 506.004578
-506.247955 100.492149 63.982090
-4.232555 100.492149 64.096809
-4.042101 100.492149 506.024475
-505.866333 100.492149 506.004578
""
""
"Illusion"
101002 5 0 0 101 0 60 1 101 0 0
66.983101, 18.000000, 420.373993
503.907745 100.492149 63.921486
504.138245 100.492149 506.280426
-4.042101 100.492149 506.024475
-4.232555 100.492149 64.096809
503.907745 100.492149 63.921486
""
"""#;
        let zones = Zones::parse(text).unwrap();
        assert_eq!(zones.zones.len(), 2);
        assert_eq!(zones.zones[0].name, "Mirror");
        assert_eq!(zones.zones[1].name, "Illusion");
        assert_eq!(zones.zones[0].polygon.len(), 5);
        assert_eq!(zones.zones[0].center, Some([-455.420013, 18.0, 420.273987]));
        assert!(zones.find(-250., 280.).is_some());
    }

    #[test]
    fn precinct_retains_ambience_paths_on_both_sides_of_playlist() {
        let s = "version 6\n123\n\"zone\"\n1 3 0 0 1 1 60 1 1 0\n0 0 0\n0 0 0\n1 0 0\n0 0 1\n\"ambiencestereo\\day.wav\"\n\"Music\\a.mp3\"\n\"ambiencestereo\\night.wav\"";
        let zones = Zones::parse(s).unwrap();
        assert_eq!(zones.zones[0].ambience_day, "ambiencestereo\\day.wav");
        assert_eq!(zones.zones[0].ambience_night, "ambiencestereo\\night.wav");
        assert_eq!(zones.zones[0].tracks, ["Music\\a.mp3"]);
    }
    #[test]
    fn ambience_lookup_rejects_traversal_and_wrong_roots() {
        let root = std::env::temp_dir();
        assert!(resolve_under(&root, "ambiencestereo/../secret.wav", "ambiencestereo").is_none());
        assert!(resolve_under(&root, "music/test.wav", "ambiencestereo").is_none());
    }
    #[test]
    fn loose_track_lookup_is_case_insensitive_and_directory_cached() {
        let root = std::env::temp_dir().join(format!("pw-music-index-{}", std::process::id()));
        let folder = root.join("Music/Tracks");
        fs::create_dir_all(&folder).unwrap();
        let track = folder.join("Day.ogg");
        fs::write(&track, b"fixture").unwrap();
        let mut index = DirectoryIndex::default();
        assert_eq!(
            index.resolve_under(&root, "music/tracks/day.ogg", "music"),
            Some(track.clone())
        );
        assert_eq!(
            index.resolve_under(&root, "MUSIC/TRACKS/DAY.OGG", "music"),
            Some(track)
        );
        assert_eq!(
            index.directories.len(),
            3,
            "each directory is enumerated once"
        );
        assert_eq!(
            index.resolutions.len(),
            1,
            "case-folded requests share cache keys"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn packed_music_prefers_exact_entry_then_ogg_fallback() {
        let mk = |name: &str| pck::Entry {
            name: name.into(),
            offset: 0,
            size: 0,
            compressed: 0,
        };
        let mut packed = HashMap::new();
        packed.insert("sfx/music/a.ogg".into(), mk("sfx/music/a.ogg"));
        let root = Path::new("nonexistent-test-client");
        assert!(
            matches!(resolve_track_indexed(root, &packed, "Music/a.mp3", &mut DirectoryIndex::default()), Some(Track::Packed { entry, .. }) if entry.name.ends_with(".ogg"))
        );
        packed.insert("sfx/music/a.mp3".into(), mk("sfx/music/a.mp3"));
        assert!(
            matches!(resolve_track_indexed(root, &packed, "Music/a.mp3", &mut DirectoryIndex::default()), Some(Track::Packed { entry, .. }) if entry.name.ends_with(".mp3"))
        );
    }
    #[cfg(not(windows))]
    #[test]
    fn ambience_switches_paths_and_stops_independently() {
        let root = std::env::temp_dir().join(format!("pw-audio-test-{}", std::process::id()));
        let maps = root.join("maps/world");
        let audio = root.join("music/ambiencestereo");
        fs::create_dir_all(&maps).unwrap();
        fs::create_dir_all(&audio).unwrap();
        let text = "version 6\n123\n\"zone\"\n1 4 0 0 1 0 60 1 1 0\n0 0 0\n-1 0 -1\n1 0 -1\n1 0 1\n-1 0 1\n\"ambiencestereo\\day.wav\"\n\"ambiencestereo\\night.wav\"";
        let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        fs::write(maps.join("precinct.clt"), bytes).unwrap();
        fs::write(audio.join("day.wav"), []).unwrap();
        fs::write(audio.join("night.wav"), []).unwrap();
        let mut player = AmbiencePlayer::new(&root);
        player.update(true, 0.5, 0., 0., false);
        assert!(player.status().contains("day.wav"));
        player.update(true, 0.2, 0., 0., true);
        assert!(player.status().contains("night.wav"));
        player.update(false, 0.2, 0., 0., true);
        assert_eq!(player.status(), "Ambience off");
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(not(windows))]
    #[test]
    fn missing_music_and_archive_errors_remain_visible() {
        let root = std::env::temp_dir().join(format!("pw-missing-audio-{}", std::process::id()));
        let mut player = MusicPlayer::new(&root);
        player.zones = Some(Zones::parse("version 6\n123\n\"zone\"\n1 4 0 0 1 1 60 1 1 0\n0 0 0\n-1 0 -1\n1 0 -1\n1 0 1\n-1 0 1\n\"\"\n\"Music\\missing.mp3\"\n\"\"").unwrap());
        player.zone_error = None;
        player.update(true, 0.5, 0., 0.);
        assert!(player.diagnostic().contains("sfx.pck unavailable"));
        assert!(player.diagnostic().contains("Music\\missing.mp3"));
        player.update(false, 0.5, 0., 0.);
        assert!(!player.diagnostic().contains("missing.mp3"));
    }
    #[cfg(not(windows))]
    #[test]
    fn missing_precinct_error_survives_first_update() {
        let root = std::env::temp_dir().join(format!("pw-missing-precinct-{}", std::process::id()));
        let mut music = MusicPlayer::new(&root);
        let mut ambience = AmbiencePlayer::new(&root);
        music.update(true, 0.5, 0., 0.);
        ambience.update(true, 0.5, 0., 0., false);
        assert!(music.diagnostic().contains("Music zones unavailable"));
        assert!(ambience.status().contains("Ambience zones unavailable"));
    }
    #[test]
    fn invalid_header_rejected() {
        assert!(Zones::parse("version 5\n1\n").is_err())
    }
}

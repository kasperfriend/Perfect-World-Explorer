//! Authentic character-action SFX (`EventType: 101`) from `sfx.pck`.
//!
//! The animation clock owns event timing. This module selects an authored file,
//! applies its authored volume/pitch ranges, and plays it through the existing
//! Windows rodio backend. Random variation selection and non-spatial local
//! playback are Explorer behavior, not a claim of bit-identical client mixing.

use crate::{
    modules::designer::catalog::{Class, Gender},
    music, pck,
};
#[cfg(windows)]
use std::time::{Duration, Instant};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

pub const BODY_CHANNEL: u8 = 0;
pub const MOUNT_CHANNEL: u8 = 1;
pub const FLIGHT_CHANNEL: u8 = 2;
pub const WEAPON_CHANNEL: u8 = 3;
pub const MOVEMENT_CHANNEL: u8 = 4;

/// CECHostPlayer::UpdateMoveSound selection.
///
/// The table is `configs/sound.txt`, event id then quoted path, and it is read
/// from the client's `configs.pck` rather than reproduced by name. It used to be
/// reconstructed here from example filenames because the copy of the archive in
/// hand had no such member; the 1.8.4 `configs.pck` has one, 67 rows, and it is
/// the authority. The built-in names below remain as the fallback for a client
/// whose archive lacks the table, and the two are known to agree on the land,
/// swim, horse, bear, puma, dinosaur and qilin ids.
///
/// The table also states ids the name rule never could: 106 and 109 and 136 and
/// 139 are empty strings (that class and gender have no footstep), 170-173 are
/// the fox and tiger forms, and 212-217 are a frog, a penguin and a rhinoceros
/// sharing a walk and a run file each.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MovementSound {
    pub id: u16,
    pub path: String,
}

/// One row of `configs/sound.txt`: an event id and the path it plays.
///
/// An empty path is a real row, not a missing one: it says that combination is
/// silent in the client, so it is kept and resolves to no sound rather than
/// falling through to a guessed name.
pub fn parse_sound_table(text: &str) -> HashMap<u16, String> {
    let mut table = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let Some((id, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(id) = id.trim().parse::<u16>() else {
            continue;
        };
        let rest = rest.trim();
        let path = rest
            .strip_prefix('"')
            .and_then(|r| r.split_once('"').map(|(p, _)| p))
            .unwrap_or(rest);
        table.insert(id, path.replace('\\', "/"));
    }
    table
}

/// The `configs/sound.txt` member of `configs.pck`, when the client ships it.
///
/// A failure here is not fatal -- the built-in names still play -- so the reason
/// is returned for the status line rather than raised as an error.
pub fn load_sound_table(element: &Path) -> (HashMap<u16, String>, Option<String>) {
    let path = element.join("configs.pck");
    let mut file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => return (HashMap::new(), Some(format!("{}: {e}", path.display()))),
    };
    let entries = match pck::directory(&mut file) {
        Ok(e) => e,
        Err(e) => return (HashMap::new(), Some(format!("{}: {e}", path.display()))),
    };
    let Some(entry) = entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case("configs/sound.txt"))
    else {
        return (
            HashMap::new(),
            Some(format!("{} has no configs/sound.txt", path.display())),
        );
    };
    let bytes = match pck::read_entry(&mut file, entry) {
        Ok(b) => b,
        Err(e) => return (HashMap::new(), Some(format!("configs/sound.txt: {e}"))),
    };
    let (text, _, bad) = encoding_rs::GBK.decode(&bytes);
    if bad {
        return (
            HashMap::new(),
            Some("configs/sound.txt is not valid GBK".into()),
        );
    }
    let table = parse_sound_table(&text);
    if table.is_empty() {
        return (HashMap::new(), Some("configs/sound.txt has no rows".into()));
    }
    (table, None)
}

impl MovementSound {
    /// The path this id plays, from the client's own table when it ships one.
    ///
    /// `table` is `configs/sound.txt`; a hit wins, including a hit on an empty
    /// path, which the client writes for a class that has no footstep. A miss
    /// falls back to the built-in name rule, which is the only option for a
    /// client whose archive omits the table.
    pub fn from_table(id: u16, table: &HashMap<u16, String>) -> Option<Self> {
        match table.get(&id) {
            Some(path) if path.is_empty() => None,
            Some(path) => Some(Self {
                id,
                path: path.clone(),
            }),
            None => None,
        }
    }

    /// The built-in rule, then the client's table when the table has the row.
    ///
    /// The rule exists to *name an id*, so the table can be consulted at all;
    /// the path it produces is discarded whenever the table answers, including
    /// when the table's answer is "silent".
    pub fn land_in_table(
        class: Class,
        gender: Gender,
        running: bool,
        mount_sound_type: Option<u32>,
        table: &HashMap<u16, String>,
    ) -> Option<Self> {
        let built_in = Self::land(class, gender, running, mount_sound_type)?;
        if table.is_empty() {
            return Some(built_in);
        }
        Self::from_table(built_in.id, table)
    }

    pub fn water_in_table(camera_above: bool, table: &HashMap<u16, String>) -> Option<Self> {
        let built_in = Self::water(camera_above);
        if table.is_empty() {
            return Some(built_in);
        }
        Self::from_table(built_in.id, table)
    }

    pub fn land(
        class: Class,
        gender: Gender,
        running: bool,
        mount_sound_type: Option<u32>,
    ) -> Option<Self> {
        let offset = if running { 1u16 } else { 0 };
        let (id, stem) = if let Some(kind) = mount_sound_type {
            let stem = match kind {
                0 => "pethorse",
                1 => "petbear",
                2 => "petpuma",
                3 => "petdinasaur",
                4 => "petqilin",
                _ => return None,
            };
            (200 + kind as u16 * 2 + offset, stem)
        } else {
            let index = (class.index() * 2 + gender.index()) as u16;
            let stem = if class == Class::Barbarian {
                "beasta"
            } else {
                match gender {
                    Gender::Male => "malea",
                    Gender::Female => "femalea",
                }
            };
            ((if running { 130 } else { 100 }) + index, stem)
        };
        Some(Self {
            id,
            path: format!(
                "sfx/character/footstep/land{}{stem}.wav",
                if running { "run" } else { "walk" }
            ),
        })
    }

    pub fn water(camera_above: bool) -> Self {
        let (id, file) = if camera_above {
            (160, "swim.wav")
        } else {
            (161, "float.wav")
        };
        Self {
            id,
            path: format!("sfx/character/footstep/{file}"),
        }
    }

    fn cue(&self) -> SoundCue {
        SoundCue {
            channel: MOVEMENT_CHANNEL,
            event: self.id as u64,
            paths: vec![self.path.clone()],
            volume_min: 100,
            volume_max: 100,
            pitch_min: 0.0,
            pitch_max: 0.0,
            percent: 1.0,
            looping: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SoundCue {
    pub channel: u8,
    pub event: u64,
    pub paths: Vec<String>,
    pub volume_min: u32,
    pub volume_max: u32,
    pub pitch_min: f32,
    pub pitch_max: f32,
    pub percent: f32,
    pub looping: bool,
}

#[derive(Clone, Debug)]
pub enum SoundCommand {
    Play(SoundCue),
    Stop { channel: u8, event: u64 },
    StopChannel(u8),
    StopAll,
}

fn movement_commands(
    previous: &Option<MovementSound>,
    requested: &Option<MovementSound>,
) -> Vec<SoundCommand> {
    if previous == requested {
        return Vec::new();
    }
    let mut commands = vec![SoundCommand::StopChannel(MOVEMENT_CHANNEL)];
    if let Some(sound) = requested {
        commands.push(SoundCommand::Play(sound.cue()));
    }
    commands
}

#[derive(Clone, Debug)]
enum Asset {
    Packed(pck::Entry),
    Loose(PathBuf),
}

#[derive(Clone, Debug)]
struct Play {
    channel: u8,
    event: u64,
    label: String,
    asset: Asset,
    gain: f32,
    speed: f32,
    looping: bool,
}

#[derive(Clone, Debug)]
enum WorkerCommand {
    Play(Play),
    Stop { channel: u8, event: u64 },
    StopChannel(u8),
    StopAll,
}

struct Request {
    enabled: bool,
    volume: f32,
    commands: Vec<WorkerCommand>,
}

pub struct CharacterAudio {
    element: PathBuf,
    packed: HashMap<String, pck::Entry>,
    resolved: HashMap<String, Option<Asset>>,
    catalog_error: Option<String>,
    worker_error: Option<String>,
    missing: HashSet<String>,
    status: String,
    enabled: bool,
    volume: f32,
    cues: u64,
    random: u32,
    last_enabled: bool,
    last_volume: f32,
    movement: Option<MovementSound>,
    #[cfg(windows)]
    tx: Option<std::sync::mpsc::Sender<Request>>,
    #[cfg(windows)]
    rx: Option<std::sync::mpsc::Receiver<Option<String>>>,
}

impl Default for CharacterAudio {
    fn default() -> Self {
        Self {
            element: PathBuf::new(),
            packed: HashMap::new(),
            resolved: HashMap::new(),
            catalog_error: None,
            worker_error: None,
            missing: HashSet::new(),
            status: "Character sounds off".into(),
            enabled: false,
            volume: 0.5,
            cues: 0,
            random: 0x51f15eED,
            last_enabled: false,
            last_volume: -1.,
            movement: None,
            #[cfg(windows)]
            tx: None,
            #[cfg(windows)]
            rx: None,
        }
    }
}

impl CharacterAudio {
    pub fn new(element: &Path) -> Self {
        let catalog = std::fs::File::open(element.join("sfx.pck"))
            .map_err(|e| e.to_string())
            .and_then(|mut file| pck::directory(&mut file));
        let catalog_error = catalog.as_ref().err().cloned();
        let packed = catalog
            .unwrap_or_default()
            .into_iter()
            .map(|entry| (entry.name.to_lowercase(), entry))
            .collect();
        #[cfg(windows)]
        let (tx, rx) = {
            let (tx, input) = std::sync::mpsc::channel();
            let (output, rx) = std::sync::mpsc::channel();
            let archive = element.join("sfx.pck");
            std::thread::Builder::new()
                .name("walker-sounds".into())
                .spawn(move || worker(input, output, archive))
                .expect("character sound worker");
            (Some(tx), Some(rx))
        };
        Self {
            element: element.into(),
            packed,
            resolved: HashMap::new(),
            catalog_error,
            status: "Character action sounds ready".into(),
            #[cfg(windows)]
            tx,
            #[cfg(windows)]
            rx,
            ..Self::default()
        }
    }

    fn random(&mut self) -> u32 {
        // A local deterministic stream avoids adding a global RNG dependency.
        // Native file/TTL/random-sequence parity is not claimed.
        let mut x = self.random;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.random = x;
        x
    }

    fn resolve(&mut self, path: &str) -> Option<Asset> {
        if let Some(asset) = self.resolved.get(path) {
            return asset.clone();
        }
        let asset = self
            .packed
            .get(path)
            .cloned()
            .map(Asset::Packed)
            .or_else(|| music::resolve_under(&self.element, path, "sfx").map(Asset::Loose));
        self.resolved.insert(path.into(), asset.clone());
        asset
    }

    fn prepare(&mut self, cue: SoundCue) -> Option<WorkerCommand> {
        if cue.paths.is_empty() || self.random() as f32 / u32::MAX as f32 > cue.percent {
            return None;
        }
        let path_index = self.random() as usize % cue.paths.len();
        let label = cue.paths[path_index].clone();
        let asset = self.resolve(&label);
        if asset.is_none() {
            self.missing.insert(label);
            return None;
        }
        let volume_range = cue.volume_max.saturating_sub(cue.volume_min);
        let volume = if volume_range == 0 {
            cue.volume_min
        } else {
            cue.volume_min + self.random() % (volume_range + 1)
        };
        let pitch_unit = self.random() as f32 / u32::MAX as f32;
        let pitch = cue.pitch_min + (cue.pitch_max - cue.pitch_min) * pitch_unit;
        self.cues = self.cues.wrapping_add(1);
        Some(WorkerCommand::Play(Play {
            channel: cue.channel,
            event: cue.event,
            label,
            asset: asset?,
            gain: volume as f32 / 100.,
            speed: 2.0f32.powf(pitch / 12.),
            looping: cue.looping,
        }))
    }

    pub fn update(
        &mut self,
        enabled: bool,
        volume: f32,
        commands: Vec<SoundCommand>,
        movement: Option<MovementSound>,
    ) {
        self.enabled = enabled;
        self.volume = volume.clamp(0., 1.);
        let requested_movement = if enabled { movement } else { None };
        let movement_changed = requested_movement != self.movement;
        #[cfg(windows)]
        if let Some(rx) = &self.rx {
            while let Ok(message) = rx.try_recv() {
                if enabled {
                    self.worker_error = message;
                }
            }
        }
        if !enabled {
            self.worker_error = None;
        }
        // Send only when something actually changed. The old 250 ms heartbeat
        // woke the worker four times a second even on a completely idle
        // character; the worker now retires finished sinks on its own timeout.
        if commands.is_empty()
            && !movement_changed
            && enabled == self.last_enabled
            && (self.volume - self.last_volume).abs() < 0.001
        {
            return;
        }
        self.last_enabled = enabled;
        self.last_volume = self.volume;
        let mut commands = commands;
        if movement_changed {
            // PlayMoveSound stops the previous buffer only when the selection
            // changes. An unchanged selection must keep its loop position.
            commands.extend(movement_commands(&self.movement, &requested_movement));
            self.movement = requested_movement;
        }
        self.status = if enabled {
            format!("Character sounds on; {} cue(s) triggered", self.cues)
        } else {
            "Character sounds off".into()
        };
        #[cfg(not(windows))]
        if enabled {
            self.status.push_str(" (audio playback is Windows-only)");
        }
        let commands: Vec<WorkerCommand> = if enabled {
            #[cfg(windows)]
            {
                commands
                    .into_iter()
                    .filter_map(|command| match command {
                        SoundCommand::Play(cue) => self.prepare(cue),
                        SoundCommand::Stop { channel, event } => {
                            Some(WorkerCommand::Stop { channel, event })
                        }
                        SoundCommand::StopChannel(channel) => {
                            Some(WorkerCommand::StopChannel(channel))
                        }
                        SoundCommand::StopAll => Some(WorkerCommand::StopAll),
                    })
                    .collect()
            }
            #[cfg(not(windows))]
            {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        #[cfg(windows)]
        if let Some(tx) = &self.tx {
            let _ = tx.send(Request {
                enabled,
                volume: self.volume,
                commands,
            });
        }
        #[cfg(not(windows))]
        let _ = commands;
    }

    pub fn stop(&mut self) {
        self.update(self.enabled, self.volume, vec![SoundCommand::StopAll], None);
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn diagnostic(&self) -> Option<String> {
        let mut messages = Vec::new();
        if let Some(error) = &self.catalog_error {
            messages.push(format!("sfx.pck unavailable: {error}"));
        }
        if let Some(error) = &self.worker_error {
            messages.push(error.clone());
        }
        if !self.missing.is_empty() {
            let mut paths = self.missing.iter().cloned().collect::<Vec<_>>();
            paths.sort();
            messages.push(format!(
                "Unresolved character sound(s): {}",
                paths.join(", ")
            ));
        }
        if messages.is_empty() {
            None
        } else {
            Some(messages.join("; "))
        }
    }
}

/// SFX paths are relative to the SFX root, just like native `"Sfx\\" + path`.
pub fn asset_key(path: &str) -> Option<String> {
    let path = path.trim().replace('\\', "/");
    if path.starts_with('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || part.contains(':'))
    {
        return None;
    }
    let path = path.to_lowercase();
    Some(if path.starts_with("sfx/") {
        path
    } else {
        format!("sfx/{path}")
    })
}

#[cfg(windows)]
fn worker(
    rx: std::sync::mpsc::Receiver<Request>,
    status: std::sync::mpsc::Sender<Option<String>>,
    archive: PathBuf,
) {
    use rodio::{Decoder, OutputStream, Sink, Source};
    use std::{
        io::{BufReader, Cursor},
        sync::Arc,
    };

    struct Playing {
        sink: Sink,
        gain: f32,
    }
    let mut output = None;
    let mut retry_output = Instant::now();
    let mut active: HashMap<(u8, u64), Playing> = HashMap::new();
    let mut cache: HashMap<String, Arc<[u8]>> = HashMap::new();
    let mut cached_bytes = 0usize;
    let mut failed = HashSet::<String>::new();
    loop {
        let mut request = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(request) => request,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Idle ticks own sink cleanup now that the main thread no
                // longer sends a keep-alive request every 250 ms.
                active.retain(|_, playing| !playing.sink.empty());
                continue;
            }
        };
        while let Ok(next) = rx.try_recv() {
            // Volume/enabled state is latest-wins, but animation commands are
            // edges. Coalescing whole requests would silently discard steps,
            // stops, and travel-channel cleanup produced by earlier frames.
            request.commands.extend(next.commands);
            request.enabled = next.enabled;
            request.volume = next.volume;
        }
        active.retain(|_, playing| !playing.sink.empty());
        if !request.enabled {
            for (_, playing) in active.drain() {
                playing.sink.stop();
            }
            failed.clear();
            continue;
        }
        if output.is_none() && Instant::now() >= retry_output {
            match OutputStream::try_default() {
                Ok(stream) => output = Some(stream),
                Err(error) => {
                    let _ =
                        status.send(Some(format!("Character sound output unavailable: {error}")));
                    retry_output = Instant::now() + Duration::from_secs(2);
                }
            }
        }
        let mut had_error = false;
        for command in request.commands {
            match command {
                WorkerCommand::Stop { channel, event } => {
                    if let Some(playing) = active.remove(&(channel, event)) {
                        playing.sink.stop();
                    }
                }
                WorkerCommand::StopChannel(channel) => {
                    active.retain(|(owner, _), playing| {
                        if *owner == channel {
                            playing.sink.stop();
                            false
                        } else {
                            true
                        }
                    });
                }
                WorkerCommand::StopAll => {
                    for (_, playing) in active.drain() {
                        playing.sink.stop();
                    }
                }
                WorkerCommand::Play(play) => {
                    if failed.contains(&play.label) {
                        continue;
                    }
                    if active.contains_key(&(play.channel, play.event)) {
                        continue;
                    }
                    let Some((_, handle)) = output.as_ref() else {
                        continue;
                    };
                    let bytes = if let Some(bytes) = cache.get(&play.label) {
                        Ok(bytes.clone())
                    } else {
                        let read = match &play.asset {
                            Asset::Packed(entry) => std::fs::File::open(&archive)
                                .map_err(|e| e.to_string())
                                .and_then(|mut file| pck::read_entry(&mut file, entry)),
                            Asset::Loose(path) => std::fs::read(path).map_err(|e| e.to_string()),
                        };
                        read.map(|bytes| {
                            let bytes: Arc<[u8]> = bytes.into();
                            if cached_bytes + bytes.len() > 16 * 1024 * 1024 {
                                cache.clear();
                                cached_bytes = 0;
                            }
                            if bytes.len() <= 16 * 1024 * 1024 {
                                cached_bytes += bytes.len();
                                cache.insert(play.label.clone(), bytes.clone());
                            }
                            bytes
                        })
                    };
                    match bytes.and_then(|bytes| {
                        Decoder::new(BufReader::new(Cursor::new(bytes))).map_err(|e| e.to_string())
                    }) {
                        Ok(source) => match Sink::try_new(handle) {
                            Ok(sink) => {
                                sink.set_volume((request.volume * play.gain).clamp(0., 1.));
                                sink.set_speed(play.speed.max(0.01));
                                if play.looping {
                                    sink.append(source.repeat_infinite());
                                } else {
                                    sink.append(source);
                                }
                                active.insert(
                                    (play.channel, play.event),
                                    Playing {
                                        sink,
                                        gain: play.gain,
                                    },
                                );
                            }
                            Err(error) => {
                                had_error = true;
                                let _ = status
                                    .send(Some(format!("Character sound output error: {error}")));
                            }
                        },
                        Err(error) => {
                            had_error = true;
                            failed.insert(play.label.clone());
                            let _ = status.send(Some(format!(
                                "Cannot load character sound {}: {error}",
                                play.label
                            )));
                        }
                    }
                }
            }
        }
        for playing in active.values() {
            playing
                .sink
                .set_volume((request.volume * playing.gain).clamp(0., 1.));
        }
        if !had_error {
            let _ = status.send(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table the client actually ships, decoded the way the reader decodes
    /// it: id, tab, quoted path, CRLF.
    /// `configs/sound.txt` exactly as the staged 1.8.4 `configs.pck` ships it:
    /// 67 rows in 70 lines (four blank separators between the walk, run and pet
    /// blocks), empty paths for the four silent rows 106/109/136/139, and the
    /// pet and mount rows 200-217. Copied verbatim rather than paraphrased,
    /// because a fixture that omits a row makes a real table look like a missing
    /// one -- that is how this test came to name an id the client cannot
    /// produce for the class it asked about.
    const SHIPPED_ROWS: &str = "100	\"SFX\\Character\\FootStep\\LandWalkMaleA.wav\"\r\n\\
101	\"SFX\\Character\\FootStep\\LandWalkFemaleA.wav\"\r\n\\
102	\"SFX\\Character\\FootStep\\LandWalkMaleA.wav\"\r\n\\
103	\"SFX\\Character\\FootStep\\LandWalkFemaleA.wav\"\r\n\\
104	\"SFX\\Character\\FootStep\\LandWalkMaleA.wav\"\r\n\\
105	\"SFX\\Character\\FootStep\\LandWalkFemaleA.wav\"\r\n\\
106	\"\"\r\n\\
107	\"SFX\\Character\\FootStep\\LandWalkFemaleA.wav\"\r\n\\
108	\"SFX\\Character\\FootStep\\LandWalkBeastA.wav\"\r\n\\
109	\"\"\r\n\\
110	\"SFX\\Character\\FootStep\\LandWalkMaleA.wav\"\r\n\\
111	\"SFX\\Character\\FootStep\\LandWalkFemaleA.wav\"\r\n\\
112	\"SFX\\Character\\FootStep\\LandWalkMaleA.wav\"\r\n\\
113	\"SFX\\Character\\FootStep\\LandWalkFemaleA.wav\"\r\n\\
114	\"SFX\\Character\\FootStep\\LandWalkMaleA.wav\"\r\n\\
115	\"SFX\\Character\\FootStep\\LandWalkFemaleA.wav\"\r\n\\
116	\"SFX\\Character\\FootStep\\LandWalkMaleA.wav\"\r\n\\
117	\"SFX\\Character\\FootStep\\LandWalkFemaleA.wav\"\r\n\\
118	\"SFX\\Character\\FootStep\\LandWalkMaleA.wav\"\r\n\\
119	\"SFX\\Character\\FootStep\\LandWalkFemaleA.wav\"\r\n\\
130	\"SFX\\Character\\FootStep\\LandRunMaleA.wav\"\r\n\\
131	\"SFX\\Character\\FootStep\\LandRunFemaleA.wav\"\r\n\\
132	\"SFX\\Character\\FootStep\\LandRunMaleA.wav\"\r\n\\
133	\"SFX\\Character\\FootStep\\LandRunFemaleA.wav\"\r\n\\
134	\"SFX\\Character\\FootStep\\LandRunMaleA.wav\"\r\n\\
135	\"SFX\\Character\\FootStep\\LandRunFemaleA.wav\"\r\n\\
136	\"\"\r\n\\
137	\"SFX\\Character\\FootStep\\LandRunFemaleA.wav\"\r\n\\
138	\"SFX\\Character\\FootStep\\LandRunBeastA.wav\"\r\n\\
139	\"\"\r\n\\
140	\"SFX\\Character\\FootStep\\LandRunMaleA.wav\"\r\n\\
141	\"SFX\\Character\\FootStep\\LandRunFemaleA.wav\"\r\n\\
142	\"SFX\\Character\\FootStep\\LandRunMaleA.wav\"\r\n\\
143	\"SFX\\Character\\FootStep\\LandRunFemaleA.wav\"\r\n\\
144	\"SFX\\Character\\FootStep\\LandRunMaleA.wav\"\r\n\\
145	\"SFX\\Character\\FootStep\\LandRunFemaleA.wav\"\r\n\\
146	\"SFX\\Character\\FootStep\\LandRunMaleA.wav\"\r\n\\
147	\"SFX\\Character\\FootStep\\LandRunFemaleA.wav\"\r\n\\
148	\"SFX\\Character\\FootStep\\LandRunMaleA.wav\"\r\n\\
149	\"SFX\\Character\\FootStep\\LandRunFemaleA.wav\"\r\n\\
160	\"SFX\\Character\\FootStep\\Swim.wav\"\r\n\\
161	\"SFX\\Character\\FootStep\\Float.wav\"\r\n\\
162	\"\"\r\n\\
163	\"\"\r\n\\
164	\"\"\r\n\\
170	\"SFX\\Character\\FootStep\\LandWalkFoxA.wav\"\r\n\\
171	\"SFX\\Character\\FootStep\\LandRunFoxA.wav\"\r\n\\
172	\"SFX\\Character\\FootStep\\LandWalkTigerA.wav\"\r\n\\
173	\"SFX\\Character\\FootStep\\LandRunTigerA.wav\"\r\n\\
200	\"SFX\\Character\\FootStep\\LandWalkPetHorse.wav\"\r\n\\
201	\"SFX\\Character\\FootStep\\LandRunPetHorse.wav\"\r\n\\
202	\"SFX\\Character\\FootStep\\LandWalkPetBear.wav\"\r\n\\
203	\"SFX\\Character\\FootStep\\LandRunPetBear.wav\"\r\n\\
204	\"SFX\\Character\\FootStep\\LandWalkPetPuma.wav\"\r\n\\
205	\"SFX\\Character\\FootStep\\LandRunPetPuma.wav\"\r\n\\
206	\"SFX\\Character\\FootStep\\LandWalkPetDinasaur.wav\"\r\n\\
207	\"SFX\\Character\\FootStep\\LandRunPetDinasaur.wav\"\r\n\\
208	\"SFX\\Character\\FootStep\\LandWalkPetQilin.wav\"\r\n\\
209	\"SFX\\Character\\FootStep\\LandRunPetQilin.wav\"\r\n\\
210	\"SFX\\Character\\FootStep\\LandWalkPetAuto.wav\"\r\n\\
211	\"SFX\\Character\\FootStep\\LandRunPetAuto.wav\"\r\n\\
212	\"SFX\\Character\\FootStep\\LandRunFrog.wav\"\r\n\\
213	\"SFX\\Character\\FootStep\\LandRunFrog.wav\"\r\n\\
214	\"SFX\\Character\\FootStep\\LandRunPenguin.wav\"\r\n\\
215	\"SFX\\Character\\FootStep\\LandRunPenguin.wav\"\r\n\\
216	\"SFX\\Character\\FootStep\\LandRunRhinoceros.wav\"\r\n\\
217	\"SFX\\Character\\FootStep\\LandRunRhinoceros.wav\"\r\n";

    #[test]
    fn sound_table_is_read_the_way_the_client_writes_it() {
        let table = parse_sound_table(SHIPPED_ROWS);
        assert_eq!(table.len(), 67);
        assert_eq!(
            table[&135],
            "SFX/Character/FootStep/LandRunFemaleA.wav".to_string()
        );
        // An empty quoted path is a row, not a missing one.
        assert_eq!(table[&106], "");
    }

    /// The client's table wins, including when it says the id is silent.
    #[test]
    fn the_clients_table_decides_the_path() {
        let table = parse_sound_table(SHIPPED_ROWS);
        // 135 is what the name rule picks for a female Psychic running, and the
        // table agrees. 105 is what it picks for a walking female Psychic, and
        // the table agrees there too -- but the answer comes from the table
        // either way. (This assertion used to read 101, which is a *male*
        // Blademaster's walk; the fixture was missing row 105, so the id the
        // rule produces for a female Psychic had no row to find.)
        let run = MovementSound::land_in_table(Class::Psychic, Gender::Female, true, None, &table)
            .unwrap();
        assert_eq!(run.id, 135);
        assert_eq!(run.path, "SFX/Character/FootStep/LandRunFemaleA.wav");
        let walk =
            MovementSound::land_in_table(Class::Psychic, Gender::Female, false, None, &table);
        // The table's own casing, not the built-in rule's lower-case name:
        // 105 is a real row and its answer wins.
        assert_eq!(
            walk.unwrap().path,
            "SFX/Character/FootStep/LandWalkFemaleA.wav"
        );
        // A silent row silences. 106 is a silent row and the rule's id for a
        // male Venomancer walk; ask for that id directly rather than assuming a
        // class/gender pair lands on it. (100 is the male Blademaster walk, and
        // the shipped table gives it a real path, so that lookup answers from
        // the table -- which is the case just above.)
        assert!(MovementSound::from_table(106, &table).is_none());
        assert_eq!(
            MovementSound::land_in_table(Class::Blademaster, Gender::Male, false, None, &table)
                .unwrap()
                .path,
            "SFX/Character/FootStep/LandWalkMaleA.wav"
        );
        // Without a table the built-in name still applies, so a client whose
        // archive lacks the member is not left mute.
        let fallback = MovementSound::land_in_table(
            Class::Blademaster,
            Gender::Male,
            false,
            None,
            &HashMap::new(),
        )
        .unwrap();
        // The rule's id for a male Blademaster walk is 100, and with no table
        // its own lower-case name is what a viewer gets.
        assert_eq!(fallback.id, 100);
        assert_eq!(fallback.path, "sfx/character/footstep/landwalkmalea.wav");
        // Swim and float are rows 160 and 161 in the shipped table, spelled the
        // way the table spells them rather than the way `water()` builds them.
        assert_eq!(
            MovementSound::water_in_table(true, &table).unwrap().path,
            "SFX/Character/FootStep/Swim.wav"
        );
        assert_eq!(
            MovementSound::water_in_table(false, &table).unwrap().path,
            "SFX/Character/FootStep/Float.wav"
        );
        // A mount pair is `200 + kind*2 + running`, and every one of the five
        // kinds is a row; 210/211 are the generic pair the rule cannot name.
        for (kind, stem) in [
            (0u32, "PetHorse"),
            (1, "PetBear"),
            (2, "PetPuma"),
            (3, "PetDinasaur"),
            (4, "PetQilin"),
        ] {
            let mounted =
                MovementSound::land_in_table(Class::Psychic, Gender::Female, true, Some(kind), &table)
                    .unwrap();
            assert_eq!(mounted.id, 200 + kind as u16 * 2 + 1);
            assert_eq!(
                mounted.path,
                format!("SFX/Character/FootStep/LandRun{stem}.wav")
            );
        }
        // 212 is a frog run, an id the name rule could never produce.
        assert!(MovementSound::from_table(212, &table).is_none() == false);
        assert_eq!(
            MovementSound::from_table(212, &table).unwrap().path,
            "SFX/Character/FootStep/LandRunFrog.wav"
        );
    }

    #[test]
    fn movement_selection_uses_native_ids_and_authentic_files() {
        let psychic = MovementSound::land(Class::Psychic, Gender::Female, true, None).unwrap();
        assert_eq!(psychic.id, 135);
        assert_eq!(psychic.path, "sfx/character/footstep/landrunfemalea.wav");
        let barbarian = MovementSound::land(Class::Barbarian, Gender::Male, true, None).unwrap();
        assert_eq!(barbarian.id, 138);
        assert_eq!(barbarian.path, "sfx/character/footstep/landrunbeasta.wav");
        let horse = MovementSound::land(Class::Psychic, Gender::Female, true, Some(0)).unwrap();
        assert_eq!(horse.id, 201);
        assert_eq!(horse.path, "sfx/character/footstep/landrunpethorse.wav");
        let raptor = MovementSound::land(Class::Psychic, Gender::Female, false, Some(3)).unwrap();
        assert_eq!(raptor.id, 206);
        assert_eq!(
            raptor.path,
            "sfx/character/footstep/landwalkpetdinasaur.wav"
        );
        assert_eq!(MovementSound::water(true).id, 160);
        assert_eq!(MovementSound::water(false).id, 161);
        assert!(MovementSound::land(Class::Psychic, Gender::Female, true, Some(5)).is_none());
    }

    #[test]
    fn unchanged_movement_selection_does_not_restart_its_loop() {
        let run = Some(MovementSound::land(Class::Psychic, Gender::Female, true, None).unwrap());
        assert!(movement_commands(&run, &run).is_empty());
        let swim = Some(MovementSound::water(true));
        let changed = movement_commands(&run, &swim);
        assert_eq!(changed.len(), 2);
        assert!(matches!(
            &changed[0],
            SoundCommand::StopChannel(MOVEMENT_CHANNEL)
        ));
        match &changed[1] {
            SoundCommand::Play(cue) => {
                assert_eq!(cue.channel, MOVEMENT_CHANNEL);
                assert_eq!(cue.event, 160);
                assert!(cue.looping);
            }
            _ => panic!("movement change did not start its selected loop"),
        }
        let stopped = movement_commands(&swim, &None);
        assert_eq!(stopped.len(), 1);
        assert!(matches!(
            &stopped[0],
            SoundCommand::StopChannel(MOVEMENT_CHANNEL)
        ));
    }

    #[test]
    fn authentic_sfx_paths_are_rooted_and_traversal_is_rejected() {
        assert_eq!(
            asset_key("Effect\\Player\\Step01.wav").as_deref(),
            Some("sfx/effect/player/step01.wav")
        );
        assert_eq!(
            asset_key("SFX/Effect/Swim.ogg").as_deref(),
            Some("sfx/effect/swim.ogg")
        );
        for path in ["../x.wav", "/x.wav", "C:/x.wav", "a//b.wav"] {
            assert_eq!(asset_key(path), None, "accepted {path}");
        }
    }
}

//! Experimental positional playback of authored ECWLD sound placements.
//! Timing, attenuation and voice limits are viewer approximations.
use crate::{music, pck, scene};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const MAX_VOICES: usize = 16;

// Short-range emitters are indexed in 256-unit X/Z cells. The 512-unit
// radius covers every emitter kept in the grid; longer-range sounds stay in
// a small global list so no authored sound can be missed by the broad phase.
const AUDIO_CELL: f32 = 256.0;
const AUDIO_LOCAL_RADIUS: f32 = 512.0;
const AUDIO_CELL_RADIUS: i32 = 2;

#[derive(Default)]
struct SpatialIndex {
    cells: HashMap<(i32, i32), Vec<usize>>,
    global: Vec<usize>,
}

impl SpatialIndex {
    fn cell(x: f32, z: f32) -> Option<(i32, i32)> {
        let x = (x / AUDIO_CELL).floor();
        let z = (z / AUDIO_CELL).floor();
        if !x.is_finite()
            || !z.is_finite()
            || x < i32::MIN as f32
            || x > i32::MAX as f32
            || z < i32::MIN as f32
            || z > i32::MAX as f32
        {
            return None;
        }
        Some((x as i32, z as i32))
    }

    fn new(emitters: &[Emitter]) -> Self {
        let mut index = Self::default();
        for (id, emitter) in emitters.iter().enumerate() {
            let sound = &emitter.sound;
            if sound.max_distance > AUDIO_LOCAL_RADIUS {
                index.global.push(id);
            } else if let Some(cell) = Self::cell(sound.position[0], sound.position[2]) {
                index.cells.entry(cell).or_default().push(id);
            } else {
                // Preserve correctness for unusual but finite coordinates.
                index.global.push(id);
            }
        }
        index
    }

    fn candidates(&self, pos: [f32; 3], emitter_count: usize) -> Vec<usize> {
        let Some((cx, cz)) = Self::cell(pos[0], pos[2]) else {
            return (0..emitter_count).collect();
        };
        let mut result = Vec::with_capacity(self.global.len() + 32);
        result.extend_from_slice(&self.global);
        for z in -AUDIO_CELL_RADIUS..=AUDIO_CELL_RADIUS {
            for x in -AUDIO_CELL_RADIUS..=AUDIO_CELL_RADIUS {
                if let (Some(x), Some(z)) = (cx.checked_add(x), cz.checked_add(z)) {
                    if let Some(ids) = self.cells.get(&(x, z)) {
                        result.extend_from_slice(ids);
                    }
                }
            }
        }
        result
    }
}

#[derive(Clone, Debug)]
enum Asset {
    Packed(pck::Entry),
    Loose(PathBuf),
}

#[derive(Clone, Debug)]
struct Emitter {
    sound: scene::Sound,
    asset: Option<Asset>,
}

fn asset_key(path: &str) -> Option<String> {
    let mut normalized = if path.is_ascii() {
        let mut s = String::with_capacity(path.len());
        for b in path.bytes() {
            s.push(if b == b'\\' {
                '/'
            } else {
                (b as char).to_ascii_lowercase()
            });
        }
        s
    } else {
        let mut s = path.to_lowercase();
        if s.as_bytes().contains(&b'\\') {
            unsafe {
                for b in s.as_bytes_mut() {
                    if *b == b'\\' {
                        *b = b'/';
                    }
                }
            }
        }
        s
    };
    if normalized.starts_with('/')
        || normalized
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.contains(':'))
    {
        return None;
    }
    if !normalized.starts_with("sfx/") {
        normalized.insert_str(0, "sfx/");
    }
    Some(normalized)
}

#[derive(Clone, Debug)]
struct Voice {
    index: usize,
    gain: f32,
    pan: f32,
}

/// The client gates modes 0/1/2 at the strict 0.5 day/night boundary.
/// Gain and the voice cap are deliberately not claimed as native behavior.
fn select(emitters: &[Emitter], spatial: &SpatialIndex, pos: [f32; 3], yaw: f32, night_factor: f32) -> Vec<Voice> {
    if !pos.iter().all(|v| v.is_finite()) || !yaw.is_finite() {
        return Vec::new();
    }
    let (sin, cos) = yaw.sin_cos();
    let mut result = Vec::with_capacity(MAX_VOICES.min(emitters.len()));
    for index in spatial.candidates(pos, emitters.len()) {
        let e = &emitters[index];
        let s = &e.sound;
        if e.asset.is_none()
            || !scene::effect_visible(s.mode, night_factor)
            || !s.min_distance.is_finite()
            || !s.max_distance.is_finite()
            || s.min_distance < 0.
            || s.max_distance <= 0.
            || s.max_distance < s.min_distance
        {
            continue;
        }
        let dx = s.position[0] - pos[0];
        let dy = s.position[1] - pos[1];
        let dz = s.position[2] - pos[2];
        let horiz_sq = dx * dx + dz * dz;
        let dist_sq = horiz_sq + dy * dy;
        if !dist_sq.is_finite() || dist_sq >= s.max_distance * s.max_distance {
            continue;
        }
        let distance = dist_sq.sqrt();
        let gain = if distance <= s.min_distance {
            1.
        } else {
            (s.max_distance - distance) / (s.max_distance - s.min_distance)
        };
        // Match the viewer's mirrored camera and D-key strafe direction.
        let horizontal = horiz_sq.sqrt();
        let pan = if horizontal > 0.0001 {
            ((-cos * dx - sin * dz) / horizontal).clamp(-1., 1.)
        } else {
            0.
        };
        result.push(Voice { index, gain, pan });
    }
    result.sort_unstable_by(|a, b| {
        b.gain
            .total_cmp(&a.gain)
            .then_with(|| a.index.cmp(&b.index))
    });
    result.truncate(MAX_VOICES);
    result
}

struct Request {
    enabled: bool,
    volume: f32,
    voices: Vec<Voice>,
}

pub struct WorldAudio {
    emitters: Vec<Emitter>,
    spatial: SpatialIndex,
    missing: usize,
    catalog_error: Option<String>,
    worker_error: Option<String>,
    status: String,
    last_update: Instant,
    last_enabled: bool,
    last_volume: f32,
    #[cfg(windows)]
    tx: std::sync::mpsc::Sender<Request>,
    #[cfg(windows)]
    rx: std::sync::mpsc::Receiver<Option<String>>,
}

impl WorldAudio {
    pub fn new(element: &Path, scene: &scene::Scene) -> Self {
        let catalog = std::fs::File::open(element.join("sfx.pck"))
            .map_err(|e| e.to_string())
            .and_then(|mut f| pck::directory(&mut f));
        let catalog_error = catalog.as_ref().err().cloned();
        let packed: HashMap<String, pck::Entry> = catalog
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.name.to_lowercase(), e))
            .collect();
        let mut cache = HashMap::new();
        let mut missing = 0;
        let emitters = scene
            .blocks
            .iter()
            .flat_map(|b| &b.sounds)
            .map(|sound| {
                let asset = asset_key(&sound.path).and_then(|key| {
                    cache
                        .entry(key.clone())
                        .or_insert_with(|| {
                            packed.get(&key).cloned().map(Asset::Packed).or_else(|| {
                                music::resolve_under(element, &key, "sfx").map(Asset::Loose)
                            })
                        })
                        .clone()
                });
                if asset.is_none() {
                    missing += 1;
                }
                Emitter {
                    sound: sound.clone(),
                    asset,
                }
            })
            .collect::<Vec<_>>();
        let spatial = SpatialIndex::new(&emitters);
        let status = format!(
            "{} world sound placements; {missing} unresolved",
            emitters.len()
        );
        #[cfg(windows)]
        let (tx, rx) = {
            let (tx, input) = std::sync::mpsc::channel();
            let (output, rx) = std::sync::mpsc::channel();
            let archive = element.join("sfx.pck");
            let assets = emitters.iter().map(|e| e.asset.clone()).collect();
            std::thread::Builder::new()
                .name("world-sounds".into())
                .spawn(move || worker(input, output, archive, assets))
                .expect("world sound worker");
            (tx, rx)
        };
        Self {
            emitters,
            spatial,
            missing,
            catalog_error,
            worker_error: None,
            status,
            last_update: Instant::now() - Duration::from_secs(1),
            last_enabled: false,
            last_volume: -1.,
            #[cfg(windows)]
            tx,
            #[cfg(windows)]
            rx,
        }
    }

    pub fn update(
        &mut self,
        enabled: bool,
        volume: f32,
        pos: [f32; 3],
        yaw: f32,
        night_factor: f32,
    ) {
        #[cfg(windows)]
        while let Ok(message) = self.rx.try_recv() {
            if enabled {
                self.worker_error = message;
            }
        }
        if !enabled {
            self.worker_error = None;
        }
        if self.last_update.elapsed() < Duration::from_millis(250)
            && enabled == self.last_enabled
            && (volume - self.last_volume).abs() < 0.001
        {
            return;
        }
        self.last_update = Instant::now();
        self.last_enabled = enabled;
        self.last_volume = volume;
        let voices = if enabled {
            select(&self.emitters, &self.spatial, pos, yaw, night_factor)
        } else {
            Vec::new()
        };
        self.status = if enabled {
            format!(
                "{} nearby world sound(s); {} unresolved placements",
                voices.len(),
                self.missing
            )
        } else {
            "World effects off".into()
        };
        #[cfg(not(windows))]
        if enabled {
            self.status.push_str(" (audio playback is Windows-only)");
        }
        let request = Request {
            enabled,
            volume,
            voices,
        };
        #[cfg(windows)]
        let _ = self.tx.send(request);
        #[cfg(not(windows))]
        let _ = request;
    }
    pub fn status(&self) -> &str {
        &self.status
    }
    pub fn diagnostic(&self) -> Option<String> {
        let mut messages = Vec::new();
        if let Some(e) = &self.catalog_error {
            messages.push(format!("sfx.pck unavailable: {e}"));
        }
        if let Some(e) = &self.worker_error {
            messages.push(e.clone());
        }
        if messages.is_empty() {
            None
        } else {
            Some(messages.join("; "))
        }
    }
}

#[cfg(windows)]
fn worker(
    rx: std::sync::mpsc::Receiver<Request>,
    status: std::sync::mpsc::Sender<Option<String>>,
    archive: PathBuf,
    assets: Vec<Option<Asset>>,
) {
    use rodio::{Decoder, OutputStream, SpatialSink};
    use std::{
        io::{BufReader, Cursor},
        sync::Arc,
    };
    struct Playing {
        sink: SpatialSink,
        replay_at: Option<Instant>,
    }
    let mut output = None;
    let mut retry_output = Instant::now();
    let mut active: HashMap<usize, Playing> = HashMap::new();
    let mut cache: HashMap<usize, Arc<[u8]>> = HashMap::new();
    let mut cached_bytes = 0usize;
    let mut failed = HashMap::<usize, String>::new();
    loop {
        let mut req = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(req) => req,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
        };
        while let Ok(next) = rx.try_recv() {
            req = next;
        }
        if !req.enabled {
            for (_, voice) in active.drain() {
                voice.sink.stop();
            }
            failed.clear();
            continue;
        }
        active.retain(|index, voice| {
            let keep = req.voices.iter().any(|v| v.index == *index);
            if !keep {
                voice.sink.stop();
            }
            keep
        });
        failed.retain(|index, _| req.voices.iter().any(|v| v.index == *index));
        if req.voices.is_empty() {
            let _ = status.send(None);
            continue;
        }
        if output.is_none() && Instant::now() >= retry_output {
            match OutputStream::try_default() {
                Ok(stream) => output = Some(stream),
                Err(e) => {
                    let _ = status.send(Some(format!("World sound output unavailable: {e}")));
                    retry_output = Instant::now() + Duration::from_secs(2);
                }
            }
        }
        let Some((_, handle)) = output.as_ref() else {
            continue;
        };
        let mut sink_error = false;
        for voice in &req.voices {
            if failed.contains_key(&voice.index) {
                continue;
            }
            let playback = if let Some(playing) = active.get_mut(&voice.index) {
                playing
            } else {
                // SpatialSink's distance law is bypassed with a unit-distance emitter;
                // authored distances are applied separately as an approximate gain.
                match SpatialSink::try_new(handle, [0., 0., -1.], [0.2, 0., 0.], [-0.2, 0., 0.]) {
                    Ok(sink) => active.entry(voice.index).or_insert(Playing {
                        sink,
                        replay_at: None,
                    }),
                    Err(e) => {
                        sink_error = true;
                        let _ = status.send(Some(format!("World sound output error: {e}")));
                        continue;
                    }
                }
            };
            let pan = voice.pan;
            playback
                .sink
                .set_emitter_position([pan, 0., -(1. - pan * pan).max(0.).sqrt()]);
            playback
                .sink
                .set_volume((req.volume.clamp(0., 1.) * voice.gain).clamp(0., 1.));
            if !playback.sink.empty() {
                continue;
            }
            if let Some(at) = playback.replay_at {
                if Instant::now() < at {
                    continue;
                }
            }
            let bytes = if let Some(bytes) = cache.get(&voice.index) {
                Ok(bytes.clone())
            } else {
                let read = match assets.get(voice.index).and_then(Option::as_ref) {
                    Some(Asset::Packed(entry)) => std::fs::File::open(&archive)
                        .map_err(|e| e.to_string())
                        .and_then(|mut f| pck::read_entry(&mut f, entry)),
                    Some(Asset::Loose(path)) => std::fs::read(path).map_err(|e| e.to_string()),
                    None => Err("sound asset is missing".into()),
                };
                read.map(|bytes| {
                    let bytes: Arc<[u8]> = bytes.into();
                    if cached_bytes + bytes.len() > 16 * 1024 * 1024 {
                        cache.clear();
                        cached_bytes = 0;
                    }
                    if bytes.len() <= 16 * 1024 * 1024 {
                        cached_bytes += bytes.len();
                        cache.insert(voice.index, bytes.clone());
                    }
                    bytes
                })
            };
            match bytes.and_then(|b| {
                Decoder::new(BufReader::new(Cursor::new(b))).map_err(|e| e.to_string())
            }) {
                Ok(source) => {
                    playback.sink.append(source);
                    // An approximate pause avoids machine-gunning short calls. Native scheduling is unknown.
                    playback.replay_at = None;
                }
                Err(e) => {
                    let label = match assets.get(voice.index).and_then(Option::as_ref) {
                        Some(Asset::Packed(entry)) => entry.name.clone(),
                        Some(Asset::Loose(path)) => path.display().to_string(),
                        None => format!("placement {}", voice.index),
                    };
                    let _ = status.send(Some(format!("Cannot load world sound {label}: {e}")));
                    failed.insert(voice.index, e);
                }
            }
        }
        if failed.is_empty() && !sink_error {
            let _ = status.send(None);
        }
        // Delay replay after each source finishes; initial playback is immediate.
        for playing in active.values_mut() {
            if !playing.sink.empty() {
                playing.replay_at = Some(Instant::now() + Duration::from_secs(2));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn emitter(position: [f32; 3], mode: i32) -> Emitter {
        Emitter {
            sound: scene::Sound {
                export_id: 1,
                position,
                min_distance: 5.,
                max_distance: 15.,
                reserved: 0,
                mode,
                path: "Effect/Scene/a.wav".into(),
            },
            asset: Some(Asset::Loose(PathBuf::from("unused"))),
        }
    }
    #[test]
    fn keys_reject_traversal_and_preserve_audio_extensions() {
        assert_eq!(
            asset_key("Effect\\Scene\\bird.wav").as_deref(),
            Some("sfx/effect/scene/bird.wav")
        );
        assert_eq!(
            asset_key("SFX/Music/a.ogg").as_deref(),
            Some("sfx/music/a.ogg")
        );
        for path in [
            "../secret.wav",
            "Effect/../secret.wav",
            "/sfx/a.wav",
            "C:/a.wav",
            "effect//a.wav",
        ] {
            assert!(asset_key(path).is_none());
        }
    }
    #[test]
    fn selects_by_distance_day_night_and_viewer_right() {
        let emitters = vec![
            emitter([-5., 0., 0.], 0),
            emitter([10., 0., 0.], 1),
            emitter([0., 0., 15.], 2),
        ];
        let spatial = SpatialIndex::new(&emitters);
        let day = select(&emitters, &spatial, [0., 0., 0.], 0., 0.);
        assert_eq!(day.len(), 1);
        assert_eq!(day[0].index, 0);
        assert_eq!(day[0].gain, 1.);
        assert_eq!(day[0].pan, 1.);
        let night = select(&emitters, &spatial, [0., 0., 0.], 0., 1.);
        assert_eq!(night.len(), 1);
        assert_eq!(night[0].index, 1);
        assert_eq!(night[0].gain, 0.5);
        assert_eq!(night[0].pan, -1.);
        assert!(select(&emitters, &spatial, [0., 0., 0.], 0., 0.5).is_empty());
        assert!(select(&emitters, &spatial, [f32::NAN, 0., 0.], 0., 0.).is_empty());
    }
    #[test]
    fn spatial_selection_matches_full_scan_for_short_and_long_ranges() {
        let mut emitters = Vec::new();
        for z in -12i32..=12 {
            for x in -12i32..=12 {
                let mut e = emitter([x as f32 * 173.0, (x - z) as f32 * 3.0, z as f32 * 191.0], 2);
                e.sound.max_distance = match (x + z).rem_euclid(4) {
                    0 => 64.0,
                    1 => 256.0,
                    2 => 512.0,
                    _ => 900.0,
                };
                e.sound.min_distance = 5.0;
                emitters.push(e);
            }
        }
        let indexed = SpatialIndex::new(&emitters);
        let full_scan = SpatialIndex {
            cells: HashMap::new(),
            global: (0..emitters.len()).collect(),
        };
        for pos in [
            [0.0, 0.0, 0.0],
            [512.0, 8.0, -512.0],
            [-1100.0, 30.0, 900.0],
            [2200.0, -10.0, 1700.0],
        ] {
            let a = select(&emitters, &indexed, pos, 0.73, 0.0);
            let b = select(&emitters, &full_scan, pos, 0.73, 0.0);
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(&b) {
                assert_eq!(a.index, b.index);
                assert_eq!(a.gain.to_bits(), b.gain.to_bits());
                assert_eq!(a.pan.to_bits(), b.pan.to_bits());
            }
        }
    }

    #[test]
    fn spatial_index_keeps_nearby_short_range_and_all_long_range_emitters() {
        let emitters = vec![
            emitter([510., 0., 0.], 2),
            emitter([770., 0., 0.], 2),
            emitter([0., 0., 900.], 2),
        ];
        let mut long = emitters[2].clone();
        long.sound.max_distance = 900.;
        let emitters = vec![emitters[0].clone(), emitters[1].clone(), long];
        let index = SpatialIndex::new(&emitters);
        let candidates = index.candidates([0., 0., 0.], emitters.len());
        assert!(candidates.contains(&0));
        assert!(!candidates.contains(&1));
        assert!(candidates.contains(&2));
    }

    #[test]
    fn caps_prioritizes_and_filters_unresolved_voices() {
        let mut emitters = (0..30)
            .map(|i| emitter([i as f32 / 2., 0., 0.], 2))
            .collect::<Vec<_>>();
        emitters[0].asset = None;
        let spatial = SpatialIndex::new(&emitters);
        let voices = select(&emitters, &spatial, [0., 0., 0.], 0., 0.);
        assert_eq!(voices.len(), MAX_VOICES);
        assert_eq!(voices[0].index, 1);
        assert!(voices.iter().all(|v| v.index != 0));
        assert!(voices.windows(2).all(|v| v[0].gain >= v[1].gain));
    }
}

//! Main-world map coordinates and destinations from the client.
use crate::pck;
use std::{collections::HashMap, fs, path::Path};
#[derive(Debug, Clone, PartialEq)]
pub struct Destination {
    pub id: u32,
    pub name: String,
    pub position: [f32; 3],
}
pub fn destinations(path: &Path) -> Result<Vec<Destination>, String> {
    destinations_for_world(path, &[1])
}

/// Read destinations authored for one map. `world_ids` are the instance IDs
/// from `configs/instance.txt`; an empty slice means all authored rows. The old
/// hard-coded `fields[2] == "1"` filter only found main-world targets and made
/// an instance map's own destination list invisible.
pub fn destinations_for_world(path: &Path, world_ids: &[u32]) -> Result<Vec<Destination>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        // Many instance maps have no authored teleport table. Absence means an
        // empty list; callers can provide a safe local fallback without turning
        // this optional file into a map/UI error.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if bytes.len() % 2 != 0 {
        return Err("odd UTF-16 destination file".into());
    }
    let words = bytes
        .chunks_exact(2)
        .map(|x| u16::from_le_bytes([x[0], x[1]]))
        .collect::<Vec<_>>();
    let text = String::from_utf16(&words).map_err(|e| e.to_string())?;
    parse_destinations(text.trim_start_matches('\u{feff}'), world_ids)
}
/// One authored row, or `None` for a line that carries no destination.
///
/// The client's own tables are hand-edited text and are not uniform, which is
/// why this tolerates more than one shape. These four rows are the real
/// contents of 1.5.2's and 1.8.4's `world/world_targets.txt` (89 rows each):
///
/// ```text
/// 1001\t雪霏村\t1\t"-2545.362,215.254,4116.355"\t1       name unquoted
/// 1001\t"Снежная деревня"\t1\t"-2545.362,215.254,4116.355"\t1  name quoted
/// 9047    九冕尘寰台\t161\t"-799.016,45,-282.283"\t-1        space for a tab
/// 9048\t萝会城\t\t161\t"-162.798,36,299.386"\t-1          empty column
/// ```
///
/// Fields are now found by what they hold rather than by their position:
///
/// * the position is the only field holding two commas;
/// * the name is the first non-empty field before it that is not a number, so
///   the empty column cannot be mistaken for one;
/// * the id is the leading run of digits of the first field, which is how the
///   row that used a space for a tab still yields `9047` rather than a name
///   glued to its id;
/// * the world/group column is the last integer after the name, which is the
///   column the client filters instances on.
///
/// A line with a position but no name is still an error: that is a real
/// authoring fault, and dropping it silently would hide a destination.
///
/// Refusing anything else made every one of 1.5.2's 89 rows invalid -- the old
/// reader demanded five tab-separated fields, a quoted name and a numeric
/// third -- so the main world listed no destinations at all.
fn parse_destination(line: &str, world_ids: &[u32]) -> Result<Option<Destination>, String> {
    let trimmed = line.trim().trim_start_matches('\u{feff}');
    if trimmed.is_empty() {
        return Ok(None);
    }
    let unquote = |s: &str| -> String { s.trim().trim_matches('"').trim().to_string() };
    let mut fields: Vec<String> = trimmed.split('\t').map(unquote).collect();
    // Some rows used spaces for the tab between the id and the name, so the
    // first field arrives as `9045    赢霄城`. Split the leading digits off it
    // and keep the rest as the name; a name that itself contains a space
    // survives because only the first run of whitespace is consumed.
    let first = fields[0].clone();
    let digits = first.len() - first.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0
        && digits < first.len()
        && first[digits..].starts_with(|c: char| c.is_whitespace())
    {
        let (id, rest) = first.split_at(digits);
        fields[0] = id.trim().to_string();
        fields.insert(1, rest.trim().to_string());
    }
    let Some(at) = fields.iter().position(|f| f.matches(',').count() == 2) else {
        // No position anywhere: a comment or a blank row, not a destination.
        return Ok(None);
    };
    let position = fields[at]
        .split(',')
        .map(|p| p.trim().parse::<f32>())
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .filter(|v| v.len() == 3 && v.iter().all(|x| x.is_finite()))
        .map(|v| [v[0], v[1], v[2]])
        .ok_or_else(|| format!("invalid destination coordinates {:?}", fields[at]))?;
    let name_at = fields[..at]
        .iter()
        .position(|f| {
            !f.is_empty() && f.parse::<f32>().is_err() && !f.chars().all(|c| c.is_ascii_digit())
        })
        .ok_or_else(|| format!("invalid destination name at {position:?}"))?;
    let id = fields
        .first()
        .map(|f| f.trim_start_matches(|c: char| !c.is_ascii_digit()))
        .map(|f| f.trim_end_matches(|c: char| !c.is_ascii_digit()))
        .and_then(|f| f.parse::<u32>().ok())
        .ok_or_else(|| format!("invalid destination ID in {line:?}"))?;
    // The column the client filters instances on: the last integer after the
    // name. A row that leaves it empty claims nothing and stays in every map.
    let world_id = fields[name_at + 1..]
        .iter()
        .take(at - name_at - 1)
        .filter_map(|f| f.parse::<u32>().ok())
        .next_back()
        .unwrap_or(0);
    if !world_ids.is_empty() && world_id != 0 && !world_ids.contains(&world_id) {
        return Ok(None);
    }
    Ok(Some(Destination {
        id,
        name: fields[name_at].clone(),
        position,
    }))
}
fn parse_destinations(text: &str, world_ids: &[u32]) -> Result<Vec<Destination>, String> {
    let mut result = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_destination(line, world_ids) {
            Ok(Some(destination)) => result.push(destination),
            Ok(None) => {}
            Err(error) => return Err(format!("destination line {}: {error}", number + 1)),
        }
    }
    result.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    Ok(result)
}
#[derive(Clone)]
pub struct MapSpace {
    pub width: usize,
    pub height: usize,
    pub spacing: f32,
    pub heights: crate::terrain_store::Heights,
}
impl MapSpace {
    pub fn world_size(&self) -> [f32; 2] {
        [
            self.width as f32 * self.spacing,
            self.height as f32 * self.spacing,
        ]
    }
    pub fn from_uv(&self, u: f32, v: f32) -> [f32; 2] {
        let [w, h] = self.world_size();
        [(u.clamp(0., 1.) - 0.5) * w, (0.5 - v.clamp(0., 1.)) * h]
    }
    pub fn to_uv(&self, x: f32, z: f32) -> [f32; 2] {
        let [w, h] = self.world_size();
        [x / w + 0.5, 0.5 - z / h]
    }
    pub fn ground(&self, x: f32, z: f32) -> f32 {
        let gx = (x / self.spacing + self.width as f32 * 0.5).clamp(0., (self.width - 1) as f32);
        let gy = (self.height as f32 * 0.5 - z / self.spacing).clamp(0., (self.height - 1) as f32);
        let ix = (gx as usize).min(self.width - 2);
        let iy = (gy as usize).min(self.height - 2);
        let fx = gx - ix as f32;
        let fy = gy - iy as f32;
        let mut heights = self.heights.reader();
        let a = heights.height(iy * self.width + ix);
        let b = heights.height(iy * self.width + ix + 1);
        let c = heights.height((iy + 1) * self.width + ix);
        let d = heights.height((iy + 1) * self.width + ix + 1);
        if fx + fy <= 1. {
            a + (b - a) * fx + (c - a) * fy
        } else {
            d + (c - d) * (1. - fx) + (b - d) * (1. - fy)
        }
    }
}
/// Client world-map tile name: ROW then COLUMN, `%03d` of `row * 10 + col`,
/// which is why the retail archive runs 000..107 for 11 rows x 8 columns. The
/// two-digit names in the same folder (74, 84, ...) are duplicates of 074, 084
/// and are not needed.
pub fn map_tile_name(row: u32, col: u32) -> String {
    format!("surfaces/maps/{:03}.dds", row * 10 + col)
}
/// Client minimap tile name: `%02d%02d` of ROW then COLUMN, the format string at
/// `0x565220` after `Surfaces\\MiniMaps\\`. Each tile is 256x256 DXT1.
pub fn minimap_tile_name(zone: &str, row: u32, col: u32) -> String {
    format!("surfaces/minimaps/{zone}/{row:02}{col:02}.dds")
}

/// The world is 8192x11264 units: 8x11 regions of 1024. A minimap row covers
/// 512 units, so the world spans exactly 16 x 22 tiles. The archive ships 23
/// rows (368 tiles); the 23rd lies past the southern edge and is not part of the
/// world, so it is not composited.
const MAP_ROWS: u32 = 11;
const MAP_COLS: u32 = 8;
const MINIMAP_ROWS: u32 = 22;
const MINIMAP_COLS: u32 = 16;
/// The main world's own minimap set; other zones (a01, b03, ...) are separate
/// instances with their own coordinate space.
pub const WORLD_MINIMAP_ZONE: &str = "world";

/// Composite a row-major tile grid from `surfaces.pck` into one image.
///
/// `side` is the per-tile output size: the client ships 1024px map tiles and
/// 256px minimap tiles, which at full size would be a 368 MB texture.
fn compose(
    element: &Path,
    rows: u32,
    cols: u32,
    side: u32,
    name: impl Fn(u32, u32) -> String,
) -> Result<image::RgbaImage, String> {
    compose_optional(element, rows, cols, side, |row, col| Some(name(row, col)))
}

/// Instance minimap folders can be sparse (for example `a32` ships 44 of a
/// 6x8 grid), so a missing tile stays transparent instead of rejecting the
/// entire zone's artwork.
fn compose_optional(
    element: &Path,
    rows: u32,
    cols: u32,
    side: u32,
    name: impl Fn(u32, u32) -> Option<String>,
) -> Result<image::RgbaImage, String> {
    let path = element.join("surfaces.pck");
    let mut f = fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let entries = pck::directory(&mut f)?;
    let by_name: HashMap<String, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.name.to_ascii_lowercase().replace('\\', "/"), i))
        .collect();
    let mut map = image::RgbaImage::new(cols * side, rows * side);
    for row in 0..rows {
        for col in 0..cols {
            let Some(tile_name) = name(row, col) else {
                continue;
            };
            let index = by_name
                .get(&tile_name.to_ascii_lowercase())
                .copied()
                .ok_or_else(|| format!("missing map tile {tile_name}"))?;
            let entry = &entries[index];
            let data = pck::read_entry(&mut f, entry)?;
            let tile = image::load_from_memory_with_format(&data, image::ImageFormat::Dds)
                .map_err(|e| format!("{tile_name}: {e}"))?
                .to_rgba8();
            let tile =
                image::imageops::resize(&tile, side, side, image::imageops::FilterType::Triangle);
            image::imageops::replace(&mut map, &tile, (col * side) as i64, (row * side) as i64);
        }
    }
    Ok(map)
}

/// The client's full-world map: 11 rows x 8 columns, covering exactly the
/// world's 8192x11264.
///
/// Tiles ship at 1024px (1 unit per texel). Compositing all 88 at native size
/// would be 8192x11264, which cannot be one GPU texture: wgpu's default
/// `max_texture_dimension_2d` is 8192, and the height alone is 11264. 512px per
/// tile keeps 2 units per texel - five times the detail the map page can resolve
/// at its default size - while staying inside that limit.
pub fn original_map(element: &Path) -> Result<image::RgbaImage, String> {
    compose(element, MAP_ROWS, MAP_COLS, 512, map_tile_name)
}

/// The client's own minimap for the main world, at the archive's native 256px
/// per tile: 4096x5632 covering 8192x11264, which is 2 world units per texel -
/// exactly what the game reads. The minimap HUD is sized to match, so the
/// default view is one texel per screen pixel instead of the upscale the earlier
/// 128px composite produced.
pub fn original_minimap(element: &Path) -> Result<image::RgbaImage, String> {
    compose(element, MINIMAP_ROWS, MINIMAP_COLS, 256, |row, col| {
        minimap_tile_name(WORLD_MINIMAP_ZONE, row, col)
    })
}

/// The retail full-world map tiles belong to the main world only. An instance
/// map can still use the live overhead render; do not show the mainland artwork
/// under a different coordinate space.
pub fn original_map_for_map(element: &Path, zone: &str) -> Result<image::RgbaImage, String> {
    if !zone.eq_ignore_ascii_case(WORLD_MINIMAP_ZONE) {
        return Err(format!(
            "surfaces/maps is the main world's artwork; use the Realistic style for {zone}"
        ));
    }
    original_map(element)
}

/// Find the native minimap tiles for the selected map. Instance folders use
/// `surfaces/minimaps/<zone>/RRCC.dds`, with zone-specific (and sometimes
/// sparse) grids; the main world keeps its verified 22x16 crop.
pub fn original_minimap_for_map(element: &Path, zone: &str) -> Result<image::RgbaImage, String> {
    if zone.eq_ignore_ascii_case(WORLD_MINIMAP_ZONE) {
        return original_minimap(element);
    }
    let path = element.join("surfaces.pck");
    let mut f = fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let entries = pck::directory(&mut f)?;
    let prefix = format!("surfaces/minimaps/{}/", zone.to_ascii_lowercase());
    let mut tiles = HashMap::new();
    let mut rows = 0;
    let mut cols = 0;
    for entry in &entries {
        let name = entry.name.to_ascii_lowercase().replace('\\', "/");
        let Some(file) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(stem) = file.strip_suffix(".dds") else {
            continue;
        };
        if stem.len() != 4 || !stem.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let row = stem[..2].parse::<u32>().unwrap_or(0);
        let col = stem[2..].parse::<u32>().unwrap_or(0);
        if row >= 64 || col >= 64 {
            continue;
        }
        tiles.insert((row, col), name);
        rows = rows.max(row + 1);
        cols = cols.max(col + 1);
    }
    if tiles.is_empty() {
        return Err(format!("no native minimap tiles for {zone}"));
    }
    compose_optional(element, rows, cols, 256, |row, col| {
        tiles.get(&(row, col)).cloned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row shapes the shipped `world_targets.txt` files really contain.
    ///
    /// Copied from the two authored tables, not invented. All four were
    /// rejected by the reader that required five tab-separated fields and a
    /// quoted name, which is why 1.5.2's main world listed no destinations.
    ///
    /// The two `雪霏村` rows are the same destination in the Chinese and the
    /// Russian client, so they carry different ids here even though a single
    /// client never ships both: the list is sorted by name, and two entries
    /// with one id make `by_id` ambiguous.
    #[test]
    fn hand_edited_destination_rows_still_yield_a_target() {
        let text = [
            "1001\t雪霏村\t1\t\"-2545.362,215.254,4116.355\"\t1",
            "1002\t\"Снежная деревня\"\t1\t\"-2545.362,215.254,4116.355\"\t1",
            "9047    九冕尘寰台\t161\t\"-799.016,45,-282.283\"\t-1",
            "9048\t萝会城\t\t161\t\"-162.798,36,299.386\"\t-1",
            "9045    赢霄城\t1\t\"3545.439,282,3051.461\"\t52\t",
        ]
        .join("\n");
        let list = parse_destinations(&text, &[]).unwrap();
        assert_eq!(
            list.len(),
            5,
            "every hand-edited row is a destination: {list:?}"
        );
        let by_id = |id: u32| list.iter().find(|d| d.id == id).unwrap().clone();
        assert_eq!(by_id(1001).name, "雪霏村");
        assert_eq!(by_id(1001).position, [-2545.362, 215.254, 4116.355]);
        // The quoted row keeps its text and loses only the quotes.
        assert!(list.iter().any(|d| d.name == "Снежная деревня"));
        // The space-for-tab row keeps its id as an id, not part of the name.
        assert_eq!(by_id(9047).name, "九冕尘寰台");
        assert_eq!(by_id(9047).position, [-799.016, 45., -282.283]);
        // The row with the empty column finds the position, not the gap.
        assert_eq!(by_id(9048).name, "萝会城");
        assert_eq!(by_id(9048).position, [-162.798, 36., 299.386]);
        assert_eq!(by_id(9045).position, [3545.439, 282., 3051.461]);
    }

    /// The column the client filters instances on is the one after the name,
    /// and a row that leaves it empty belongs to whatever map is loaded.
    #[test]
    fn the_world_column_selects_the_map_the_row_is_authored_for() {
        let text = [
            "1001\t雪霏村\t1\t\"0,0,0\"\t1",
            "9048\t萝会城\t\t161\t\"0,0,0\"\t-1",
            "9100\t横天阙\t161\t\"0,0,0\"\t-1",
        ]
        .join("\n");
        let ids = |worlds: &[u32]| {
            let mut v = parse_destinations(&text, worlds)
                .unwrap()
                .into_iter()
                .map(|d| d.id)
                .collect::<Vec<_>>();
            v.sort_unstable();
            v
        };
        // 9048's column is the empty one before `161`, so it claims 161: the
        // main world keeps only its own row.
        assert_eq!(ids(&[1]), [1001]);
        // The instance keeps both rows that name it.
        assert_eq!(ids(&[161]), [9048, 9100]);
        // No filter at all lists every authored row.
        assert_eq!(ids(&[]), [1001, 9048, 9100]);
    }

    /// A position with no name is an authoring fault worth reporting, and a
    /// line with no position is not a destination row at all.
    #[test]
    fn a_position_without_a_name_is_an_error_not_a_silent_drop() {
        let error = parse_destinations("1001\t\t1\t\"1,2,3\"\t1", &[]).unwrap_err();
        assert!(error.contains("invalid destination name"), "{error}");
        assert!(parse_destinations("just a comment", &[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn client_map_tile_names_follow_the_archive() {
        // Row then column, `%03d` of row*10+col: the retail folder holds
        // 000..107 for 11 rows x 8 columns plus a duplicate two-digit set.
        assert_eq!(map_tile_name(0, 0), "surfaces/maps/000.dds");
        assert_eq!(map_tile_name(0, 7), "surfaces/maps/007.dds");
        assert_eq!(map_tile_name(1, 0), "surfaces/maps/010.dds");
        assert_eq!(map_tile_name(6, 4), "surfaces/maps/064.dds");
        assert_eq!(map_tile_name(10, 7), "surfaces/maps/107.dds");
        // The minimap is `%02d%02d`, matching the `Surfaces\MiniMaps\%02d%02d`
        // format string at 0x565220.
        assert_eq!(
            minimap_tile_name("world", 0, 0),
            "surfaces/minimaps/world/0000.dds"
        );
        assert_eq!(
            minimap_tile_name("world", 21, 15),
            "surfaces/minimaps/world/2115.dds"
        );
    }

    #[test]
    fn minimap_grid_covers_the_world_exactly() {
        // 16 tiles of 512 units across 8192, 22 rows of 512 down 11264.
        assert_eq!(MINIMAP_COLS * 512, 8 * 1024);
        assert_eq!(MINIMAP_ROWS * 512, 11 * 1024);
        // The shipped 23rd row is outside the world and must not be composited.
        assert_eq!(MINIMAP_ROWS + 1, 23);
    }
    #[test]
    fn client_targets() {
        let d=parse_destinations("3801\t\"Archosaur, North District\"\t1\t\"1470.490,219.608,1180.864\"\t23\n1\t\"Other\"\t2\t\"1,2,3\"\t0", &[1]).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].position, [1470.490, 219.608, 1180.864]);
        let other = parse_destinations(
            "3801\t\"Archosaur\"\t1\t\"1,2,3\"\t23\n9001\t\"Instance\"\t101\t\"4,5,6\"\t0",
            &[101],
        )
        .unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].name, "Instance");
    }
    #[test]
    fn missing_map_targets_are_an_empty_optional_list() {
        let root = std::env::temp_dir().join(format!("pw-no-map-targets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("world_targets.txt");
        assert!(destinations_for_world(&path, &[101]).unwrap().is_empty());
        std::fs::write(&path, [0xff]).unwrap();
        assert!(destinations_for_world(&path, &[101]).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn orientation_roundtrip_and_terrain() {
        let m = MapSpace {
            width: 4,
            height: 4,
            spacing: 1.,
            heights: crate::terrain_store::Heights::from(vec![
                0., 1., 2., 3., 4., 5., 6., 7., 8., 9., 10., 11., 12., 13., 14., 15.,
            ]),
        };
        assert_eq!(m.from_uv(0., 0.), [-2., 2.]);
        assert_eq!(m.from_uv(1., 1.), [2., -2.]);
        let p = m.from_uv(0.7, 0.2);
        let uv = m.to_uv(p[0], p[1]);
        assert!((uv[0] - 0.7).abs() < 0.00001 && (uv[1] - 0.2).abs() < 0.00001);
        assert!((m.ground(-0.75, 0.75) - 6.25).abs() < 0.0001);
    }
}

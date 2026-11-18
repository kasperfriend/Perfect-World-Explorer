//! Per-snapshot geometry accounting. Entries count the unit named by their
//! class, not unique archive files or pixels. Texture fallbacks are annotations
//! on the admitted material references, not extra geometry entries.
use std::fmt::Write;

#[derive(Clone, Copy)]
pub(crate) enum Class {
    NearTerrain,
    FarTerrain,
    Trees,
    Grass,
    Buildings,
    Water,
    WorldModels,
    Walker,
    Populator,
    EffectDraws,
    EffectRoots,
}

const CLASSES: [(&str, &str); 11] = [
    ("near_terrain", "blocks"),
    ("far_terrain", "patches"),
    ("trees", "placements"),
    ("grass", "instances supplied by the native stream"),
    ("buildings", "BMD placements"),
    ("water", "authored areas"),
    ("world_models", "actors"),
    ("walker", "body/face/hanger models"),
    ("populator", "active actors"),
    (
        "effect_draws",
        "quads and posed skin draws emitted by the players",
    ),
    ("effect_roots", "scene and model event owners"),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub loaded: usize,
    pub range: usize,
    pub cutoff: usize,
    pub replaced: usize,
    pub budget: usize,
    pub pending: usize,
    pub disabled: usize,
    pub empty: usize,
    pub package: usize,
    pub unavailable: usize,
    pub invalid: usize,
    pub unsupported: usize,
    pub texture_fallback: usize,
}

impl Counts {
    pub fn skipped(self) -> usize {
        self.range
            + self.cutoff
            + self.replaced
            + self.budget
            + self.pending
            + self.disabled
            + self.empty
    }

    pub fn failed(self) -> usize {
        self.package + self.unavailable + self.invalid + self.unsupported
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Inventory {
    rows: [Counts; CLASSES.len()],
}

impl Inventory {
    pub fn combine(&self, moving: &Self) -> Self {
        // Static and moving producers own disjoint rows.
        let mut combined = *self;
        combined.rows[Class::WorldModels as usize..]
            .copy_from_slice(&moving.rows[Class::WorldModels as usize..]);
        combined
    }

    pub fn csv(&self) -> String {
        let mut out = String::from("class,loaded,skipped,failed,range,cutoff,replaced,budget,pending,disabled,empty,package,unavailable,invalid,unsupported,texture_fallback\n");
        for ((name, _), row) in CLASSES.iter().zip(self.rows) {
            writeln!(
                out,
                "{name},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                row.loaded,
                row.skipped(),
                row.failed(),
                row.range,
                row.cutoff,
                row.replaced,
                row.budget,
                row.pending,
                row.disabled,
                row.empty,
                row.package,
                row.unavailable,
                row.invalid,
                row.unsupported,
                row.texture_fallback
            )
            .unwrap();
        }
        out
    }

    pub fn labels(&self) -> impl Iterator<Item = String> + '_ {
        CLASSES
            .iter()
            .zip(self.rows.iter())
            .filter_map(|((name, unit), &row)| {
                (row != Counts::default()).then(|| {
                    format!(
                        "{name} ({unit}): {} loaded, {} skipped, {} failed; {} texture fallbacks",
                        row.loaded,
                        row.skipped(),
                        row.failed(),
                        row.texture_fallback
                    )
                })
            })
    }
}

impl std::ops::Index<Class> for Inventory {
    type Output = Counts;
    fn index(&self, class: Class) -> &Counts {
        &self.rows[class as usize]
    }
}

impl std::ops::IndexMut<Class> for Inventory {
    fn index_mut(&mut self, class: Class) -> &mut Counts {
        &mut self.rows[class as usize]
    }
}

#[cfg(test)]
#[test]
fn rows_have_stable_order_and_exclusive_reason_totals() {
    let mut static_scene = Inventory::default();
    static_scene[Class::Buildings] = Counts {
        loaded: 2,
        budget: 7,
        package: 1,
        texture_fallback: 3,
        ..Counts::default()
    };
    let mut moving = Inventory::default();
    moving[Class::WorldModels] = Counts {
        loaded: 4,
        unsupported: 5,
        range: 6,
        cutoff: 9,
        ..Counts::default()
    };
    let combined = static_scene.combine(&moving);
    assert!(combined
        .csv()
        .contains("buildings,2,7,1,0,0,0,7,0,0,0,1,0,0,0,3\n"));
    assert!(combined
        .csv()
        .contains("world_models,4,15,5,6,9,0,0,0,0,0,0,0,0,5,0\n"));
}

//! Source-backed clock for the ordinary single-base scene actions. More
//! complex combined actions fail explicitly; random/event/transition scheduling
//! is not approximated by choosing the first base.
use crate::model_asset::ModelAsset;
#[derive(Clone, Debug)]
pub struct Playback {
    action: Option<usize>,
    start: u32,
    end: u32,
    current: u32,
    remaining: i32,
    /// The authored loop budget, kept so a re-triggered action starts over with
    /// the same policy instead of the value the tick loop has decremented.
    loops: i32,
    speed: f32,
}
impl Playback {
    pub fn new(asset: &ModelAsset, combined: Option<usize>) -> Result<Self, String> {
        let Some(index) = combined else {
            return Ok(Self {
                action: None,
                start: 0,
                end: 0,
                current: 0,
                remaining: 0,
                loops: 0,
                speed: 1.,
            });
        };
        let combined = asset
            .descriptor
            .actions
            .get(index)
            .ok_or("combined action index out of range")?;
        if combined.bases.len() != 1 || combined.bases[0].start_ms.unwrap_or(0) != 0 {
            return Err(
                "multi-base/delayed combined playback requires event/transition scheduling".into(),
            );
        }
        if !matches!(combined.loops.unwrap_or(1), 1 | -1) {
            return Err("finite repeated combined playback is not implemented".into());
        }
        let base = &combined.bases[0];
        let (min, max) = combined.base_loop_bounds(0).unwrap();
        let remaining = if min == -1 || max == -1 {
            -1
        } else if min == max && min > 0 {
            min
        } else {
            return Err("random/zero combined base loop policy is not implemented".into());
        };
        let action = asset
            .model
            .actions
            .iter()
            .position(|a| a.name == base.name)
            .ok_or("combined base action is missing")?;
        let clip = &asset.model.actions[action];
        let fps = clip
            .fps
            .filter(|fps| *fps > 0)
            .unwrap_or(asset.skeleton.fps);
        if fps <= 0
            || !clip.start.is_finite()
            || !clip.end.is_finite()
            || clip.start < 0.
            || clip.end < clip.start
        {
            return Err("invalid ordinary action range/frame rate".into());
        }
        // Actual 1.4.2 0x7c5580 and 0x7c50e0: truncate each endpoint to ms,
        // then duration=end_ms-start_ms+1 (one millisecond, not one frame).
        let start = (clip.start * 1000. / fps as f32).trunc();
        let end = (clip.end * 1000. / fps as f32).trunc();
        if end >= u32::MAX as f32 {
            return Err("action millisecond range overflow".into());
        }
        let speed = asset.descriptor.render_settings().default_speed * combined.speed.unwrap_or(1.);
        if !speed.is_finite() || speed < 0. {
            return Err("negative/nonfinite ordinary playback speed is not implemented".into());
        }
        Ok(Self {
            action: Some(action),
            start: start as u32,
            end: end as u32,
            current: start as u32,
            remaining,
            loops: remaining,
            speed,
        })
    }
    /// A directly named action with the caller's own loop budget.
    ///
    /// Element 160 hands its `ModelActName` and `Loops` straight to
    /// `A3DSkinModel::PlayActionByNameDC(m_strActName, m_nLoops, 0)`, so no
    /// combined-action name is involved and the loop count is the element's,
    /// not a base action's. The range arithmetic is the same one `new` uses:
    /// each endpoint is a frame index truncated to whole milliseconds, and the
    /// loop length is `end - start + 1` (`0x7c5580`, `0x7c50e0`).
    ///
    /// `A3DSkinModelAction::Update` decrements `m_iLoopCnt` once per wrap and
    /// stops when it reaches zero, holding the last moment's pose
    /// (`dwAbsTime = GetEndTime()`), so `loops` counts whole plays: `1` plays
    /// once and holds, `-1` never stops. `loops == 0` never plays at all — see
    /// the `!iNumLoop` guard in `A3DSkinModelAction::Play` — and the caller is
    /// expected not to build a playback for it.
    pub fn from_action(
        asset: &ModelAsset,
        action: usize,
        loops: i32,
        speed: f32,
    ) -> Result<Self, String> {
        let clip = asset
            .model
            .actions
            .get(action)
            .ok_or("model action index out of range")?;
        let fps = clip
            .fps
            .filter(|fps| *fps > 0)
            .unwrap_or(asset.skeleton.fps);
        if fps <= 0
            || !clip.start.is_finite()
            || !clip.end.is_finite()
            || clip.start < 0.
            || clip.end < clip.start
        {
            return Err("invalid ordinary action range/frame rate".into());
        }
        let start = (clip.start * 1000. / fps as f32).trunc();
        let end = (clip.end * 1000. / fps as f32).trunc();
        if end >= u32::MAX as f32 {
            return Err("action millisecond range overflow".into());
        }
        if loops == 0 {
            return Err("a zero loop count never plays".into());
        }
        if !speed.is_finite() || speed < 0. {
            return Err("negative/nonfinite ordinary playback speed is not implemented".into());
        }
        Ok(Self {
            action: Some(action),
            start: start as u32,
            end: end as u32,
            current: start as u32,
            remaining: loops,
            loops,
            speed,
        })
    }
    /// CECModel::Tick 0x6d0e4f–0x6d0e62 truncates delta*speed on EACH tick;
    /// it does not retain fractional milliseconds across ticks.
    pub fn tick(&mut self, delta_ms: u32) -> Result<(), String> {
        let scaled = delta_ms as f32 * self.speed;
        if !scaled.is_finite() || scaled >= u32::MAX as f32 {
            return Err("model tick overflow".into());
        }
        self.advance(scaled as u32);
        Ok(())
    }
    fn advance(&mut self, delta: u32) {
        if self.action.is_none() || self.remaining == 0 || delta == 0 {
            return;
        }
        let absolute = self.current as u64 + delta as u64;
        if absolute <= self.end as u64 {
            self.current = absolute as u32;
            return;
        }
        let duration = self.end as u64 - self.start as u64 + 1;
        if duration == 1 {
            self.current = self.end;
            if self.remaining > 0 {
                self.remaining = 0;
            }
            return;
        }
        let wraps = (absolute - self.start as u64) / duration;
        if self.remaining > 0 && wraps >= self.remaining as u64 {
            self.remaining = 0;
            self.current = self.end;
        } else {
            if self.remaining > 0 {
                self.remaining -= wraps as i32;
            }
            self.current = (absolute - wraps * duration) as u32;
        }
    }
    /// A stopped finite action keeps the last sampled bone state; a scene
    /// that failed both combined-name lookups has no sampled action at all.
    pub fn sample(&self) -> Option<(usize, f32)> {
        self.action.map(|a| (a, self.current as f32 * 0.001))
    }
    pub fn stopped(&self) -> bool {
        self.action.is_none() || self.remaining == 0
    }
    /// Starts the same action over with its authored loop budget.
    ///
    /// The client re-triggers a scene actor's idle when the clip finishes, so a
    /// finite authored loop count schedules one pass and not a pose the actor
    /// holds for the rest of the session. Nothing happens for an action that has
    /// no clip at all.
    pub fn restart(&mut self) {
        if self.action.is_some() {
            self.current = self.start;
            self.remaining = self.loops;
        }
    }
}
/// The scene loader imports MSVCRT.dll!rand at IAT 0x8d5478 and applies
/// `% 1000` before its first model tick. Own a reproducible viewer seed; do not
/// claim to reproduce the client's unrelated process-wide RNG consumption.
pub struct SceneRandom {
    state: u32,
}
impl SceneRandom {
    pub fn new(seed: u32) -> Self {
        Self { state: seed }
    }
    pub fn initial_tick(&mut self) -> u32 {
        self.state = self.state.wrapping_mul(214013).wrapping_add(2531011);
        ((self.state >> 16) & 0x7fff) % 1000
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scene_initial_tick_matches_msvcrt_sequence_and_range() {
        let mut random = SceneRandom::new(1);
        assert_eq!(
            (0..5).map(|_| random.initial_tick()).collect::<Vec<_>>(),
            [41, 467, 334, 500, 169]
        );
        for _ in 0..10000 {
            assert!(random.initial_tick() < 1000);
        }
    }
    fn asset() -> ModelAsset {
        let (mut asset, _) = crate::model_draw_fixture::fixture();
        asset.model.actions.push(crate::ecmodel::Action {
            name: "base".into(),
            start: 15.,
            end: 30.,
            fps: None,
            track_file: String::new(),
            joints: vec![],
        });
        asset.skeleton.fps = 15;
        asset
            .descriptor
            .actions
            .push(crate::ecmodel::CombinedAction {
                name: "visible".into(),
                loops: Some(1),
                speed: Some(1.),
                bases: vec![crate::ecmodel::BaseAction {
                    name: "base".into(),
                    start_ms: Some(0),
                    loop_min: Some(-1),
                    loop_max: Some(-1),
                }],
            });
        asset
    }
    #[test]
    fn inclusive_millisecond_endpoint_not_extra_frame() {
        let a = asset();
        let mut p = Playback::new(&a, Some(0)).unwrap();
        assert_eq!(p.sample(), Some((0, 1.)));
        p.tick(1000).unwrap();
        assert_eq!(p.current, 2000);
        p.tick(1).unwrap();
        assert_eq!(p.current, 1000);
        p.tick(4005).unwrap();
        assert_eq!(p.current, 1001);
    }
    #[test]
    fn a_scene_actor_restarts_its_idle_instead_of_holding_the_last_pose() {
        let mut a = asset();
        a.descriptor.actions[0].bases[0].loop_min = Some(1);
        a.descriptor.actions[0].bases[0].loop_max = Some(1);
        let mut p = Playback::new(&a, Some(0)).unwrap();
        p.tick(2001).unwrap();
        assert!(p.stopped());
        p.restart();
        assert!(!p.stopped());
        assert_eq!(p.sample(), Some((0, 1.)));
        // A playback with no clip at all stays inert.
        let mut p = Playback::new(&a, None).unwrap();
        p.restart();
        assert!(p.stopped());
        assert_eq!(p.sample(), None);
    }
    #[test]
    fn finite_loop_stops_at_last_pose_and_failed_dispatch_does_not_play() {
        let mut a = asset();
        a.descriptor.actions[0].bases[0].loop_min = Some(2);
        a.descriptor.actions[0].bases[0].loop_max = Some(2);
        let mut p = Playback::new(&a, Some(0)).unwrap();
        p.tick(2001).unwrap();
        assert_eq!(p.current, 2000);
        assert!(!p.stopped());
        p.tick(1).unwrap();
        assert!(p.stopped());
        assert_eq!(p.current, 2000);
        p.tick(9999).unwrap();
        assert_eq!(p.current, 2000);
        let mut p = Playback::new(&a, None).unwrap();
        p.tick(9999).unwrap();
        assert_eq!(p.sample(), None);
    }
    #[test]
    fn forced_infinite_base_and_per_tick_speed_truncation() {
        let mut a = asset();
        a.descriptor.actions[0].loops = Some(-1);
        a.descriptor.actions[0].bases[0].loop_min = Some(1);
        a.descriptor.actions[0].bases[0].loop_max = Some(1);
        a.descriptor.default_speed = Some(0.5);
        let mut p = Playback::new(&a, Some(0)).unwrap();
        for _ in 0..10 {
            p.tick(1).unwrap();
        }
        assert_eq!(p.current, 1000);
        p.tick(4004).unwrap();
        assert_eq!(p.current, 1000);
        assert!(!p.stopped());
    }
    #[test]
    fn a_named_model_action_counts_whole_plays_and_holds_the_last_frame() {
        let a = asset();
        // `Loops: -1`: 174 of the shipped element-160 blocks. It never stops.
        let mut p = Playback::from_action(&a, 0, -1, 1.).unwrap();
        for _ in 0..5 {
            p.tick(2000).unwrap();
            assert!(!p.stopped());
        }
        // `Loops: 1`: 72 blocks, plus the 201 blocks whose model predates the
        // key and uses the constructor's default of 1. One play, then the end
        // frame is held: `dwAbsTime = m_pCore->GetEndTime()`.
        let mut p = Playback::from_action(&a, 0, 1, 1.).unwrap();
        p.tick(1000).unwrap();
        assert_eq!(p.current, 2000, "the inclusive 1.000 s endpoint");
        assert!(!p.stopped());
        p.tick(1).unwrap();
        assert!(p.stopped());
        assert_eq!(p.sample(), Some((0, 2.)));
        // `A3DSkinModelAction::Play` refuses `iNumLoop == 0` outright.
        assert!(Playback::from_action(&a, 0, 0, 1.).is_err());
        assert!(Playback::from_action(&a, 9, 1, 1.).is_err());
    }

    #[test]
    fn zero_span_and_invalid_or_complex_policies_are_explicit() {
        let mut a = asset();
        a.model.actions[0].end = 15.;
        let mut p = Playback::new(&a, Some(0)).unwrap();
        p.tick(9999).unwrap();
        assert_eq!(p.current, 1000);
        a.descriptor.actions[0].bases[0].start_ms = Some(1);
        assert!(Playback::new(&a, Some(0)).is_err());
        a.descriptor.actions[0].bases[0].start_ms = Some(0);
        a.descriptor.default_speed = Some(-1.);
        assert!(Playback::new(&a, Some(0)).is_err());
    }
}

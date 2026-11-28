//! Navigation-path movement for populated actors.
//!
//! The client's own placement table (`data/task_npc.data`) carries a position
//! and nothing else: no waypoint list, no facing, no dwell time. Patrol routes
//! are server `ai` data, so the real waypoints reach this module through the
//! population's route table — empty for the shipped 1.4.2 client, populated
//! once a server `npcgen.data`/`ai` export supplies them. Nothing here invents
//! a path: a spawn without a route stands still.
//!
//! The rule is the one the game plays back: walk each leg at the authored
//! speed, hold at a waypoint for its authored dwell time, and either stop or
//! continue looping at the last waypoint.

use glam::Vec3;

/// One authored waypoint. `wait` is the dwell time in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Waypoint {
    pub position: [f32; 3],
    pub wait: f32,
}

/// A real navigation path: at least two waypoints, walked at `speed` units per
/// second, optionally looping back to the first waypoint.
#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub waypoints: Vec<Waypoint>,
    pub speed: f32,
    pub looping: bool,
}

impl Route {
    pub fn validate(&self) -> Result<(), String> {
        if self.waypoints.len() < 2 {
            return Err(format!(
                "a navigation path needs at least two waypoints, got {}",
                self.waypoints.len()
            ));
        }
        if !self.speed.is_finite() || self.speed <= 0. {
            return Err(format!("invalid navigation speed {}", self.speed));
        }
        for (index, waypoint) in self.waypoints.iter().enumerate() {
            if !waypoint.position.iter().all(|value| value.is_finite()) {
                return Err(format!("navigation waypoint {index} is not finite"));
            }
            if !waypoint.wait.is_finite() || waypoint.wait < 0. {
                return Err(format!(
                    "navigation waypoint {index} has an invalid dwell time"
                ));
            }
        }
        Ok(())
    }
}

/// What an actor did during the last step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Motion {
    Walking,
    Waiting,
    Finished,
}

/// Cursor over one [`Route`], advanced by the frame delta.
#[derive(Clone, Debug, PartialEq)]
pub struct Follow {
    waypoints: Vec<Waypoint>,
    speed: f32,
    looping: bool,
    /// Index of the waypoint being walked towards.
    target: usize,
    wait: f32,
    finished: bool,
}

impl Follow {
    pub fn new(route: &Route) -> Result<Self, String> {
        route.validate()?;
        Ok(Self {
            waypoints: route.waypoints.clone(),
            speed: route.speed,
            looping: route.looping,
            target: 1,
            wait: route.waypoints[0].wait,
            finished: false,
        })
    }

    /// Route speed in world units per second, used to select a matching
    /// walk or run cycle for a rendered actor.
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Moves `position` along the route. Height is left to the caller so the
    /// actor can follow the terrain it is standing on.
    pub fn step(&mut self, position: &mut Vec3, delta: f32) -> Motion {
        if !delta.is_finite() || delta <= 0. {
            return if self.finished {
                Motion::Finished
            } else {
                Motion::Waiting
            };
        }
        // The dwell is served even on the final waypoint: the actor stands at
        // the end of its path for the authored time before it stops.
        if self.wait > 0. {
            self.wait = (self.wait - delta).max(0.);
            return Motion::Waiting;
        }
        if self.finished {
            return Motion::Finished;
        }
        let target = Vec3::from(self.waypoints[self.target].position);
        let dx = target.x - position.x;
        let dz = target.z - position.z;
        let dist_sq = dx * dx + dz * dz;
        let travel = self.speed * delta;
        if dist_sq <= travel * travel || dist_sq < 1e-8 {
            position.x = target.x;
            position.z = target.z;
            self.arrive();
            return Motion::Walking;
        }
        let scale = travel / dist_sq.sqrt();
        let step = Vec3::new(dx * scale, 0., dz * scale);
        position.x += step.x;
        position.z += step.z;
        Motion::Walking
    }

    fn arrive(&mut self) {
        self.wait = self.waypoints[self.target].wait;
        let next = self.target + 1;
        if next < self.waypoints.len() {
            self.target = next;
        } else if self.looping {
            self.target = 0;
        } else {
            self.finished = true;
        }
    }
}

/// Facing yaw for a horizontal direction.
///
/// The module's `transform` maps the actor's local `+Z` (the convention
/// `walker::character::character_transform` uses) onto `(sin yaw, 0, -cos yaw)`,
/// so yaw 0 faces world `-Z`.
pub fn yaw_towards(direction: Vec3) -> f32 {
    direction.x.atan2(-direction.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> Route {
        Route {
            waypoints: vec![
                Waypoint {
                    position: [0., 0., 0.],
                    wait: 1.,
                },
                Waypoint {
                    position: [10., 0., 0.],
                    wait: 0.5,
                },
            ],
            speed: 5.,
            looping: false,
        }
    }

    #[test]
    fn a_route_needs_two_finite_waypoints_and_a_positive_speed() {
        let mut single = route();
        single.waypoints.truncate(1);
        assert!(single.validate().is_err());
        let mut zero_speed = route();
        zero_speed.speed = 0.;
        assert!(zero_speed.validate().is_err());
        let mut nan = route();
        nan.waypoints[1].position[0] = f32::NAN;
        assert!(nan.validate().is_err());
        let mut negative_wait = route();
        negative_wait.waypoints[0].wait = -1.;
        assert!(negative_wait.validate().is_err());
        assert!(route().validate().is_ok());
    }

    #[test]
    fn the_actor_waits_then_walks_a_leg_at_the_authored_speed() {
        let mut follow = Follow::new(&route()).unwrap();
        let mut position = Vec3::ZERO;
        assert_eq!(follow.step(&mut position, 0.5), Motion::Waiting);
        assert_eq!(follow.step(&mut position, 0.5), Motion::Waiting);
        assert_eq!(position, Vec3::ZERO);
        // Waiting is over: one second at five units per second covers half a leg.
        assert_eq!(follow.step(&mut position, 1.), Motion::Walking);
        assert!((position.x - 5.).abs() < 1e-4);
        assert_eq!(follow.step(&mut position, 1.), Motion::Walking);
        assert!((position.x - 10.).abs() < 1e-4);
        // The endpoint holds for its own dwell time, then the path ends.
        assert_eq!(follow.step(&mut position, 0.25), Motion::Waiting);
        assert_eq!(follow.step(&mut position, 0.25), Motion::Waiting);
        assert_eq!(follow.step(&mut position, 1.), Motion::Finished);
        assert_eq!(follow.step(&mut position, 1.), Motion::Finished);
        assert_eq!(position, Vec3::new(10., 0., 0.));
    }

    #[test]
    fn a_looping_route_returns_to_its_first_waypoint() {
        let mut loop_route = route();
        loop_route.looping = true;
        let mut follow = Follow::new(&loop_route).unwrap();
        let mut position = Vec3::ZERO;
        let mut motions = Vec::new();
        for _ in 0..64 {
            motions.push(follow.step(&mut position, 0.5));
        }
        // A looping route never ends, and it always travels on its own legs.
        assert!(!motions.contains(&Motion::Finished));
        assert!((-1e-3..10.5).contains(&position.x));
        assert!(motions.contains(&Motion::Walking));
    }

    #[test]
    fn yaw_round_trips_through_the_transform_direction() {
        use std::f32::consts::{FRAC_PI_2, PI};
        assert!(yaw_towards(Vec3::NEG_Z).abs() < 1e-6);
        assert!((yaw_towards(Vec3::Z) - PI).abs() < 1e-6);
        assert!((yaw_towards(Vec3::X) - FRAC_PI_2).abs() < 1e-6);
        assert!((yaw_towards(Vec3::NEG_X) + FRAC_PI_2).abs() < 1e-6);
        // Every yaw maps to one horizontal direction and comes back to the
        // same angle modulo a full turn (`yaw_towards` returns atan2's range).
        let turn = 2. * PI;
        for step in -32..=32 {
            let yaw = step as f32 * 0.25;
            let direction = Vec3::new(yaw.sin(), 0., -yaw.cos());
            let turned = yaw_towards(direction);
            let wrapped = (turned - yaw + PI).rem_euclid(turn) - PI;
            assert!(wrapped.abs() < 1e-4, "yaw {yaw} round-tripped to {turned}");
        }
    }
}

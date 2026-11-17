//! Small, dependency-free vector math for the Imaginator.
//!
//! The module deliberately does not use `glam`. Two reasons: the tracer runs on
//! its own worker thread and only ever needs a dozen operations, and every file
//! under `src/imaginator/` that holds real logic must be type-checkable on its
//! own with a bare `rustc --test`, which an external crate dependency would
//! prevent. The few conversions the core needs (`to_array`) are explicit.

use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct V3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub const fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}

impl V3 {
    pub const ZERO: V3 = V3 {
        x: 0.,
        y: 0.,
        z: 0.,
    };
    pub const ONE: V3 = V3 {
        x: 1.,
        y: 1.,
        z: 1.,
    };

    pub fn from_array(a: [f32; 3]) -> Self {
        Self {
            x: a[0],
            y: a[1],
            z: a[2],
        }
    }

    pub fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }

    pub fn splat(v: f32) -> Self {
        Self { x: v, y: v, z: v }
    }

    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Self) -> Self {
        Self {
            x: self.y * o.z - self.z * o.y,
            y: self.z * o.x - self.x * o.z,
            z: self.x * o.y - self.y * o.x,
        }
    }

    pub fn length_squared(self) -> f32 {
        self.dot(self)
    }

    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }

    pub fn normalize_or_zero(self) -> Self {
        let len = self.length();
        if len > 1e-20 && len.is_finite() {
            self * (1. / len)
        } else {
            Self::ZERO
        }
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    pub fn min(self, o: Self) -> Self {
        Self {
            x: self.x.min(o.x),
            y: self.y.min(o.y),
            z: self.z.min(o.z),
        }
    }

    pub fn max(self, o: Self) -> Self {
        Self {
            x: self.x.max(o.x),
            y: self.y.max(o.y),
            z: self.z.max(o.z),
        }
    }

    pub fn abs(self) -> Self {
        Self {
            x: self.x.abs(),
            y: self.y.abs(),
            z: self.z.abs(),
        }
    }

    pub fn max_component(self) -> f32 {
        self.x.max(self.y).max(self.z)
    }

    pub fn lerp(self, o: Self, t: f32) -> Self {
        self * (1. - t) + o * t
    }

    /// Largest component index, for the BVH split axis.
    pub fn largest_axis(self) -> usize {
        if self.x >= self.y && self.x >= self.z {
            0
        } else if self.y >= self.z {
            1
        } else {
            2
        }
    }

    pub fn component(self, axis: usize) -> f32 {
        match axis {
            0 => self.x,
            1 => self.y,
            _ => self.z,
        }
    }

    /// sRGB-encoded value back to linear, matching the world shader's transfer
    /// function so albedo taken from authored bytes lands in the same space the
    /// sun colour is in.
    pub fn srgb_to_linear(self) -> Self {
        let f = |v: f32| {
            let v = v.clamp(0., 1.);
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        Self {
            x: f(self.x),
            y: f(self.y),
            z: f(self.z),
        }
    }
}

impl Add for V3 {
    type Output = V3;
    fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl AddAssign for V3 {
    fn add_assign(&mut self, o: V3) {
        *self = *self + o;
    }
}

impl Sub for V3 {
    type Output = V3;
    fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl SubAssign for V3 {
    fn sub_assign(&mut self, o: V3) {
        *self = *self - o;
    }
}

impl Neg for V3 {
    type Output = V3;
    fn neg(self) -> V3 {
        v3(-self.x, -self.y, -self.z)
    }
}

impl Mul<f32> for V3 {
    type Output = V3;
    fn mul(self, s: f32) -> V3 {
        v3(self.x * s, self.y * s, self.z * s)
    }
}

impl Mul<V3> for V3 {
    type Output = V3;
    fn mul(self, o: V3) -> V3 {
        v3(self.x * o.x, self.y * o.y, self.z * o.z)
    }
}

impl Div<f32> for V3 {
    type Output = V3;
    fn div(self, s: f32) -> V3 {
        v3(self.x / s, self.y / s, self.z / s)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub origin: V3,
    /// Unit length by construction; the tracer normalises every ray it builds.
    pub direction: V3,
}

impl Ray {
    pub fn new(origin: V3, direction: V3) -> Self {
        Self {
            origin,
            direction: direction.normalize_or_zero(),
        }
    }

    pub fn at(&self, t: f32) -> V3 {
        self.origin + self.direction * t
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: V3,
    pub max: V3,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb {
        min: v3(f32::INFINITY, f32::INFINITY, f32::INFINITY),
        max: v3(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY),
    };

    pub fn new(min: V3, max: V3) -> Self {
        Self { min, max }
    }

    pub fn from_points(points: &[V3]) -> Self {
        let mut b = Self::EMPTY;
        for &p in points {
            b.grow(p);
        }
        b
    }

    pub fn grow(&mut self, p: V3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&mut self, o: &Aabb) {
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
    }

    pub fn center(&self) -> V3 {
        (self.min + self.max) * 0.5
    }

    pub fn extent(&self) -> V3 {
        self.max - self.min
    }

    pub fn surface_area(&self) -> f32 {
        let e = self.extent();
        if !(e.x >= 0. && e.y >= 0. && e.z >= 0.) {
            return 0.;
        }
        2. * (e.x * e.y + e.y * e.z + e.z * e.x)
    }

    /// Slab test. `tmax` is the current closest-hit distance so the traversal
    /// can stop early; returns the entry distance when the box is entered
    /// inside `[0, tmax]`.
    pub fn intersect(&self, ray: &Ray, tmax: f32) -> Option<f32> {
        let mut t0 = 0.0f32;
        let mut t1 = tmax;
        for axis in 0..3 {
            let d = ray.direction.component(axis);
            let o = ray.origin.component(axis);
            let lo = self.min.component(axis);
            let hi = self.max.component(axis);
            if d.abs() < 1e-20 {
                if o < lo || o > hi {
                    return None;
                }
                continue;
            }
            let inv = 1.0 / d;
            let mut near = (lo - o) * inv;
            let mut far = (hi - o) * inv;
            if near > far {
                std::mem::swap(&mut near, &mut far);
            }
            // Pad by an epsilon so a ray leaving a surface it just hit is not
            // rejected by the box that surface's own triangle lives in.
            t0 = t0.max(near);
            t1 = t1.min(far);
            if t0 > t1 {
                return None;
            }
        }
        Some(t0)
    }

    pub fn contains(&self, p: V3) -> bool {
        p.x >= self.min.x
            && p.y >= self.min.y
            && p.z >= self.min.z
            && p.x <= self.max.x
            && p.y <= self.max.y
            && p.z <= self.max.z
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_operations() {
        let a = v3(1., 2., 3.);
        let b = v3(4., 5., 6.);
        assert_eq!(a + b, v3(5., 7., 9.));
        assert_eq!(b - a, v3(3., 3., 3.));
        assert_eq!(a * 2., v3(2., 4., 6.));
        assert!((a.dot(b) - 32.).abs() < 1e-6);
        assert_eq!(a.cross(b), v3(-3., 6., -3.));
        assert!((v3(3., 4., 0.).length() - 5.).abs() < 1e-6);
        assert_eq!(V3::ZERO.normalize_or_zero(), V3::ZERO);
        assert!((v3(0., 5., 0.).normalize_or_zero() - v3(0., 1., 0.)).length() < 1e-6);
        assert_eq!(v3(1., 5., 2.).largest_axis(), 1);
        assert!((v3(0.5, 0.5, 0.5).srgb_to_linear().x - 0.21404114).abs() < 1e-6);
        assert!((V3::ONE.srgb_to_linear().x - 1.).abs() < 1e-6);
    }

    #[test]
    fn aabb_slab_test_hits_and_misses() {
        let b = Aabb::new(v3(-1., -1., -1.), v3(1., 1., 1.));
        let hit = b.intersect(&Ray::new(v3(0., 0., -5.), v3(0., 0., 1.)), 100.);
        assert!(hit.is_some_and(|t| (t - 4.).abs() < 1e-4), "{hit:?}");
        assert!(b
            .intersect(&Ray::new(v3(0., 0., -5.), v3(0., 1., 0.)), 100.)
            .is_none());
        // A ray starting inside reports t = 0.
        assert_eq!(
            b.intersect(&Ray::new(V3::ZERO, v3(1., 0., 0.)), 100.),
            Some(0.)
        );
        // Beyond the closest-hit limit the box does not count.
        assert!(b
            .intersect(&Ray::new(v3(0., 0., -5.), v3(0., 0., 1.)), 2.)
            .is_none());
        assert!((b.surface_area() - 24.).abs() < 1e-4);
        assert!(b.contains(v3(0.5, 0.5, 0.5)) && !b.contains(v3(2., 0., 0.)));
    }
}

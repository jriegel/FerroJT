//! Axis aligned bounding volumes.

use serde::{Deserialize, Serialize};

/// An axis aligned bounding box in model units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    /// Minimum corner.
    pub min: [f32; 3],
    /// Maximum corner.
    pub max: [f32; 3],
}

impl Aabb {
    /// A degenerate box that is the identity for [`Aabb::union`].
    pub const EMPTY: Aabb = Aabb {
        min: [f32::MAX; 3],
        max: [f32::MIN; 3],
    };

    /// A box from two corners.
    pub fn new(min: [f32; 3], max: [f32; 3]) -> Self {
        Self { min, max }
    }

    /// `true` if the box contains no points.
    pub fn is_empty(&self) -> bool {
        self.min[0] > self.max[0]
    }

    /// The smallest box containing both boxes.
    pub fn union(self, other: Aabb) -> Aabb {
        Aabb {
            min: [
                self.min[0].min(other.min[0]),
                self.min[1].min(other.min[1]),
                self.min[2].min(other.min[2]),
            ],
            max: [
                self.max[0].max(other.max[0]),
                self.max[1].max(other.max[1]),
                self.max[2].max(other.max[2]),
            ],
        }
    }

    /// Extend the box with a single point.
    pub fn push_point(&mut self, p: [f32; 3]) {
        for axis in self.min.iter_mut().zip(self.max.iter_mut()).zip(p) {
            let ((min, max), point) = axis;
            *min = min.min(point);
            *max = max.max(point);
        }
    }

    /// Centre of the box.
    pub fn center(&self) -> [f32; 3] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }

    /// Edge lengths of the box.
    pub fn extent(&self) -> [f32; 3] {
        [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_of_empty_is_identity() {
        let box_ = Aabb::new([-1.0, 0.0, 2.0], [3.0, 4.0, 5.0]);
        assert_eq!(Aabb::EMPTY.union(box_), box_);
    }

    #[test]
    fn push_point_grows_box() {
        let mut box_ = Aabb::EMPTY;
        box_.push_point([1.0, 1.0, 1.0]);
        box_.push_point([-1.0, 2.0, 0.0]);
        assert_eq!(box_.min, [-1.0, 1.0, 0.0]);
        assert_eq!(box_.max, [1.0, 2.0, 1.0]);
        assert_eq!(box_.center(), [0.0, 1.5, 0.5]);
    }
}

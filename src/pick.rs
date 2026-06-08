//! Ray casting against the node boxes, for mouse picking.
//!
//! Port of `FSNode::intersect` and `Dir::find_intersection`/`pick`.

use crate::fstree::Tree;
use macroquad::prelude::Vec3;

const ERROR_MARGIN: f32 = 1e-4;

pub struct Ray {
    pub origin: Vec3,
    pub dir: Vec3,
}

/// Slab-style ray/AABB test. Returns the nearest positive hit distance `t`.
fn intersect_box(ray: &Ray, center: Vec3, half: Vec3) -> Option<f32> {
    let min = center - half;
    let max = center + half;

    let mut tmin = f32::NEG_INFINITY;
    let mut tmax = f32::INFINITY;

    for axis in 0..3 {
        let o = component(ray.origin, axis);
        let d = component(ray.dir, axis);
        let lo = component(min, axis);
        let hi = component(max, axis);

        if d.abs() < ERROR_MARGIN {
            // Ray parallel to this slab: miss if origin is outside it.
            if o < lo || o > hi {
                return None;
            }
        } else {
            let inv = 1.0 / d;
            let mut t1 = (lo - o) * inv;
            let mut t2 = (hi - o) * inv;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            tmin = tmin.max(t1);
            tmax = tmax.min(t2);
            if tmin > tmax {
                return None;
            }
        }
    }

    let t = if tmin > ERROR_MARGIN { tmin } else { tmax };
    if t > ERROR_MARGIN {
        Some(t)
    } else {
        None
    }
}

fn component(v: Vec3, axis: usize) -> f32 {
    match axis {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

impl Tree {
    /// Find the nearest node hit by `ray`, searching every box in the arena.
    pub fn find_intersection(&self, ray: &Ray) -> Option<usize> {
        let mut nearest_t = f32::INFINITY;
        let mut nearest = None;
        for (i, node) in self.nodes.iter().enumerate() {
            if let Some(t) = intersect_box(ray, node.vis_pos, node.vis_size) {
                if t < nearest_t {
                    nearest_t = t;
                    nearest = Some(i);
                }
            }
        }
        nearest
    }

    /// Update the hover selection. Returns true if the selection changed.
    pub fn pick(&mut self, ray: &Ray) -> bool {
        let node = self.find_intersection(ray);
        let changed = self.selection != node;
        self.selection = node;
        changed
    }
}

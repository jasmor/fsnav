//! Transient visual effects played when a file operation completes: a burst of
//! particles plus an optional "flying box" tween from source to destination.

use macroquad::prelude::*;

pub struct Particle {
    pub pos: Vec3,
    pub vel: Vec3,
    pub life: f32, // remaining seconds
    pub max_life: f32,
    pub color: Color,
}

/// A box that animates from `from` to `to` (for copy/move).
pub struct FlyingBox {
    pub from: Vec3,
    pub to: Vec3,
    pub size: Vec3,
    pub t: f32, // 0..=1
    pub speed: f32,
    pub color: Color,
}

#[derive(Default)]
pub struct Effects {
    pub particles: Vec<Particle>,
    pub flyers: Vec<FlyingBox>,
}

impl Effects {
    /// Green upward burst for a copy.
    pub fn spawn_copy(&mut self, from: Vec3, to: Vec3, size: Vec3) {
        self.flyers.push(FlyingBox {
            from,
            to,
            size,
            t: 0.0,
            speed: 1.6,
            color: Color::new(0.45, 0.95, 0.6, 0.9),
        });
        self.burst(to, Color::new(0.45, 0.95, 0.6, 1.0), 36);
    }

    /// Blue arc for a move (source disappears, lands at destination).
    pub fn spawn_move(&mut self, from: Vec3, to: Vec3, size: Vec3) {
        self.flyers.push(FlyingBox {
            from,
            to,
            size,
            t: 0.0,
            speed: 1.6,
            color: Color::new(0.5, 0.75, 1.0, 0.9),
        });
        self.burst(from, Color::new(0.5, 0.75, 1.0, 1.0), 24);
        self.burst(to, Color::new(0.5, 0.75, 1.0, 1.0), 24);
    }

    /// Red implosion / scatter for a trash.
    pub fn spawn_trash(&mut self, at: Vec3) {
        self.burst(at, Color::new(1.0, 0.4, 0.35, 1.0), 64);
    }

    fn burst(&mut self, at: Vec3, color: Color, count: usize) {
        for i in 0..count {
            // deterministic pseudo-random spread without an rng dependency
            let a = i as f32 * 2.399_963; // golden angle
            let r = (i as f32 / count as f32).sqrt();
            let speed = 1.5 + r * 2.5;
            let vel = vec3(a.cos() * r, 0.6 + r, a.sin() * r).normalize() * speed;
            self.particles.push(Particle {
                pos: at,
                vel,
                life: 0.9,
                max_life: 0.9,
                color,
            });
        }
    }

    /// Advance all effects; drop finished ones. Returns true if anything is
    /// still animating (so the UI keeps requesting frames).
    pub fn update(&mut self, dt: f32) -> bool {
        for p in &mut self.particles {
            p.life -= dt;
            p.vel.y -= 4.0 * dt; // gravity
            p.pos += p.vel * dt;
        }
        self.particles.retain(|p| p.life > 0.0);

        for f in &mut self.flyers {
            f.t = (f.t + f.speed * dt).min(1.0);
        }
        self.flyers.retain(|f| f.t < 1.0);

        !self.particles.is_empty() || !self.flyers.is_empty()
    }

    /// Draw the 3D parts (call while the 3D camera is active).
    pub fn draw(&self) {
        for f in &self.flyers {
            // smoothstep + a little arc height
            let t = f.t * f.t * (3.0 - 2.0 * f.t);
            let mut pos = f.from.lerp(f.to, t);
            pos.y += (t * std::f32::consts::PI).sin() * 1.5; // hop
            let mut col = f.color;
            col.a *= 1.0 - f.t * 0.3;
            draw_cube(pos, f.size, None, col);
            draw_cube_wires(pos, f.size, Color::new(col.r, col.g, col.b, col.a));
        }
        for p in &self.particles {
            let frac = (p.life / p.max_life).clamp(0.0, 1.0);
            let mut col = p.color;
            col.a = frac;
            let s = 0.06 * frac + 0.02;
            draw_cube(p.pos, vec3(s, s, s), None, col);
        }
    }
}

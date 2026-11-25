use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpringCheckpoint {
    pub current_position: f64,
    pub target_position: f64,
    pub stiffness: f64,
    pub damping: f64,
    pub mass: f64,
    pub soft: bool,
    pub time: f64,
    pub from: f64,
    pub velocity: f64,
    pub initial_velocity: f64,
    pub overdamped: bool,
    pub delta: f64,
    pub af: f64,
    pub leftover: f64,
    pub dfm: f64,
    pub dm: f64,
}

impl SpringCheckpoint {
    pub(crate) fn validate(&self) -> Result<()> {
        let values = [
            self.current_position, self.target_position, self.stiffness, self.damping,
            self.mass, self.time, self.from, self.velocity, self.initial_velocity,
            self.delta, self.af, self.leftover, self.dfm, self.dm,
        ];
        if values.iter().any(|value| !value.is_finite()) {
            bail!("spring checkpoint contains non-finite state");
        }
        if self.stiffness <= 0.0 || self.damping < 0.0 || self.mass <= 0.0 || self.time < 0.0 {
            bail!("spring checkpoint has invalid parameters");
        }
        let expected_overdamped = self.soft
            || self.damping >= 2.0 * (self.stiffness * self.mass).sqrt();
        if self.overdamped != expected_overdamped {
            bail!("spring checkpoint has inconsistent damping mode");
        }
        if (self.delta - (self.target_position - self.from)).abs() > 1e-9 {
            bail!("spring checkpoint has inconsistent displacement");
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Spring {
    current_position: f64,
    target_position: f64,
    stiffness: f64,
    damping: f64,
    mass: f64,
    soft: bool,
    time: f64,
    from: f64,
    velocity: f64,
    initial_velocity: f64,
    overdamped: bool,
    delta: f64,
    af: f64,
    leftover: f64,
    dfm: f64,
    dm: f64,
}

impl Spring {
    pub fn save_checkpoint(&self) -> SpringCheckpoint {
        SpringCheckpoint {
            current_position: self.current_position,
            target_position: self.target_position,
            stiffness: self.stiffness,
            damping: self.damping,
            mass: self.mass,
            soft: self.soft,
            time: self.time,
            from: self.from,
            velocity: self.velocity,
            initial_velocity: self.initial_velocity,
            overdamped: self.overdamped,
            delta: self.delta,
            af: self.af,
            leftover: self.leftover,
            dfm: self.dfm,
            dm: self.dm,
        }
    }

    pub fn restore_checkpoint(&mut self, checkpoint: &SpringCheckpoint) -> Result<()> {
        checkpoint.validate()?;
        self.current_position = checkpoint.current_position;
        self.target_position = checkpoint.target_position;
        self.stiffness = checkpoint.stiffness;
        self.damping = checkpoint.damping;
        self.mass = checkpoint.mass;
        self.soft = checkpoint.soft;
        self.time = checkpoint.time;
        self.from = checkpoint.from;
        self.velocity = checkpoint.velocity;
        self.initial_velocity = checkpoint.initial_velocity;
        self.overdamped = checkpoint.overdamped;
        self.delta = checkpoint.delta;
        self.af = checkpoint.af;
        self.leftover = checkpoint.leftover;
        self.dfm = checkpoint.dfm;
        self.dm = checkpoint.dm;
        Ok(())
    }

    fn recompute_solution(&mut self) {
        self.delta = self.target_position - self.from;
        self.overdamped = self.soft || self.damping >= 2.0 * (self.stiffness * self.mass).sqrt();
        if self.overdamped {
            self.af = -(self.stiffness / self.mass).sqrt();
            self.leftover = -self.af * self.delta - self.initial_velocity;
            self.dfm = 0.0;
            self.dm = 0.0;
        } else {
            let df = (4.0 * self.mass * self.stiffness - self.damping * self.damping).sqrt();
            self.leftover =
                (self.damping * self.delta - 2.0 * self.mass * self.initial_velocity) / df;
            self.dfm = df / (2.0 * self.mass);
            self.dm = -self.damping / (2.0 * self.mass);
            self.af = 0.0;
        }
    }

    fn sample(&self, time: f64) -> f64 {
        if self.overdamped {
            self.target_position - (self.delta + time * self.leftover) * (time * self.af).exp()
        } else {
            self.target_position
                - ((time * self.dfm).cos() * self.delta + (time * self.dfm).sin() * self.leftover)
                    * (time * self.dm).exp()
        }
    }
}

impl Spring {
    pub fn new(position: f64) -> Self {
        Self::with_parameters(position, 100.0, 10.0, 1.0, false)
    }

    pub fn with_parameters(
        position: f64,
        stiffness: f64,
        damping: f64,
        mass: f64,
        soft: bool,
    ) -> Self {
        let mut spring = Self {
            current_position: position,
            target_position: position,
            stiffness,
            damping,
            velocity: 0.0,
            initial_velocity: 0.0,
            mass,
            soft,
            time: 0.0,
            from: position,
            overdamped: false,
            delta: 0.0,
            af: 0.0,
            leftover: 0.0,
            dfm: 0.0,
            dm: 0.0,
        };
        spring.recompute_solution();
        spring
    }

    pub fn set_parameters(&mut self, stiffness: f64, damping: f64, mass: f64, soft: bool) {
        if (self.stiffness - stiffness).abs() < f64::EPSILON
            && (self.damping - damping).abs() < f64::EPSILON
            && (self.mass - mass).abs() < f64::EPSILON
            && self.soft == soft
        {
            return;
        }
        self.stiffness = stiffness;
        self.damping = damping;
        self.mass = mass;
        self.soft = soft;
        self.from = self.current_position;
        self.initial_velocity = self.velocity;
        self.time = 0.0;
        self.recompute_solution();
    }

    pub fn set_position(&mut self, position: f64) {
        self.current_position = position;
        self.target_position = position;
        self.from = position;
        self.velocity = 0.0;
        self.initial_velocity = 0.0;
        self.time = 0.0;
        self.recompute_solution();
    }

    pub fn set_target_position(&mut self, position: f64) {
        if (self.target_position - position).abs() < 0.001 {
            return;
        }
        self.target_position = position;
        self.from = self.current_position;
        self.initial_velocity = self.velocity;
        self.time = 0.0;
        self.recompute_solution();
    }

    pub fn update(&mut self, dt: f64) {
        if (self.current_position - self.target_position).abs() < 0.01 && self.velocity.abs() < 0.01
        {
            self.set_position(self.target_position);
            return;
        }
        self.time += dt.max(0.0);
        self.current_position = self.sample(self.time);
        let h = 1e-4;
        self.velocity = (self.sample(self.time + h) - self.sample(self.time - h)) / (2.0 * h);
        if (self.current_position - self.target_position).abs() < 0.01 && self.velocity.abs() < 0.01
        {
            self.set_position(self.target_position);
        }
    }

    pub fn current_position(&self) -> f64 {
        self.current_position
    }

    pub fn current_velocity(&self) -> f64 {
        self.velocity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_converges_with_default_policy() {
        let mut spring = Spring::with_parameters(0.0, 220.0, 33.0, 1.0, false);
        spring.set_target_position(1.0);
        for _ in 0..120 {
            spring.update(1.0 / 60.0);
        }
        assert!((spring.current_position() - 1.0).abs() < 0.01);
    }

    #[test]
    fn matches_closed_form_samples() {
        let mut spring = Spring::with_parameters(0.0, 4.0, 1.0, 1.0, false);
        spring.set_target_position(1.0);
        let expected = [0.0, 0.11286328, 0.39294515];
        for (index, expected) in expected.into_iter().enumerate() {
            if index > 0 {
                spring.update(0.25);
            }
            assert!(
                (spring.current_position() - expected).abs() < 0.00001,
                "sample {index}: {}",
                spring.current_position()
            );
        }
    }

    #[test]
    fn overdamped_motion_does_not_overshoot() {
        let mut spring = Spring::with_parameters(0.0, 100.0, 30.0, 1.0, false);
        spring.set_target_position(1.0);
        let mut previous = 0.0;
        for _ in 0..300 {
            spring.update(1.0 / 60.0);
            assert!(spring.current_position() >= previous - 1e-9);
            assert!(spring.current_position() <= 1.0 + 1e-9);
            previous = spring.current_position();
        }
    }
}

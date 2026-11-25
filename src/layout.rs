use crate::lrc::{LyricLine, scan_end_ms};
use crate::spring::{Spring, SpringCheckpoint};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

const POS_STIFFNESS: f64 = 90.0;
const POS_DAMPING: f64 = 15.0;
const BLUR_STIFFNESS: f64 = 144.0;
const BLUR_DAMPING: f64 = 24.0;
const MAX_BLUR_LEVEL: usize = 7;
const MIN_ANTICIPATION_MS: u64 = 180;
const MAX_ANTICIPATION_MS: u64 = 700;
const MIN_INTERLUDE_MS: u64 = 4_000;
const INTERLUDE_FLOAT_DELAY_MS: u64 = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interlude {
    pub next_idx: usize,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutCheckpoint {
    pub schema_version: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub layout_height: f64,
    pub line_count: usize,
    pub pos_y: Vec<SpringCheckpoint>,
    pub scale: Vec<SpringCheckpoint>,
    pub blur: Vec<SpringCheckpoint>,
    pub active_idx: usize,
    pub focus_idx: usize,
    pub playing_indices: Vec<usize>,
    pub highlighted_indices: Vec<usize>,
    pub previous_t_ms: Option<u64>,
    pub transition_ms: u64,
    pub stagger_delays_ms: Vec<u64>,
    pub group_heights: Vec<f64>,
    pub group_gap: f64,
    pub interlude_slot_height: f64,
    pub initialized: bool,
    pub interlude: Option<Interlude>,
}

impl LayoutCheckpoint {
    pub fn validate_schema(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("unsupported layout checkpoint schema version {}", self.schema_version);
        }
        if !self.layout_height.is_finite() || self.layout_height <= 0.0 {
            bail!("invalid checkpoint layout height");
        }
        let unbound = self.width == 0 && self.height == 0 && self.fps == 0;
        if !unbound && (self.width == 0 || self.height == 0 || self.fps == 0) {
            bail!("checkpoint renderer dimensions must be positive");
        }
        let n = self.line_count;
        if self.pos_y.len() != n
            || self.scale.len() != n
            || self.blur.len() != n
            || self.stagger_delays_ms.len() != n
            || self.group_heights.len() != n
        {
            bail!("checkpoint vector lengths do not match line count");
        }
        if n == 0 {
            if self.active_idx != 0 || self.focus_idx != 0 {
                bail!("empty checkpoint has nonzero active index");
            }
        } else if self.active_idx >= n || self.focus_idx >= n {
            bail!("checkpoint index out of range");
        }
        for indices in [&self.playing_indices, &self.highlighted_indices] {
            if indices.iter().any(|index| *index >= n)
                || indices.windows(2).any(|pair| pair[0] >= pair[1])
            {
                bail!("checkpoint indices are not sorted and unique");
            }
        }
        for spring in self.pos_y.iter().chain(&self.scale).chain(&self.blur) {
            spring.validate()?;
        }
        if self.group_heights.iter().any(|value| !value.is_finite() || *value < 0.0)
            || !self.group_gap.is_finite()
            || self.group_gap < 0.0
            || !self.interlude_slot_height.is_finite()
            || self.interlude_slot_height < 0.0
        {
            bail!("checkpoint has invalid layout dimensions");
        }
        if let Some(interlude) = self.interlude {
            if interlude.next_idx >= n || interlude.start_ms > interlude.end_ms {
                bail!("checkpoint has invalid interlude");
            }
        }
        Ok(())
    }
}

fn interlude_between(lines: &[LyricLine], next_idx: usize) -> Option<Interlude> {
    let next = lines.get(next_idx)?;
    let start_ms = next_idx
        .checked_sub(1)
        .and_then(|index| lines.get(index).map(|line| line.end_ms))
        .unwrap_or(0);
    let end_ms = next.start_ms;
    (end_ms.saturating_sub(start_ms) >= MIN_INTERLUDE_MS).then_some(Interlude {
        next_idx,
        start_ms,
        end_ms,
    })
}

pub fn interlude_for(lines: &[LyricLine], t_ms: u64) -> Option<Interlude> {
    (0..lines.len()).find_map(|next_idx| {
        let interlude = interlude_between(lines, next_idx)?;
        (interlude.start_ms <= t_ms && t_ms < interlude.end_ms).then_some(interlude)
    })
}

#[derive(Debug)]
pub struct Layout {
    pub pos_y: Vec<Spring>,
    pub scale: Vec<Spring>,
    pub blur: Vec<Spring>,
    active_idx: usize,
    focus_idx: usize,
    playing_indices: Vec<usize>,
    highlighted_indices: Vec<usize>,
    previous_t_ms: Option<u64>,
    transition_ms: u64,
    stagger_delays_ms: Vec<u64>,
    height: f64,
    initialized: bool,
    // Total height occupied by each lyric group, including its text.
    group_heights: Vec<f64>,
    // Additional empty space between adjacent groups; excludes group_heights.
    group_gap: f64,
    interlude_slot_height: f64,
    interlude: Option<Interlude>,
}

impl Layout {
    pub fn new(
        lines: &[LyricLine],
        height: f32,
        group_heights: &[f64],
        group_gap: f64,
        interlude_slot_height: f64,
    ) -> Self {
        let active_idx = 0;
        let center = height as f64 * 0.35;
        let group_heights = lines
            .iter()
            .enumerate()
            .map(|(index, _)| group_heights.get(index).copied().unwrap_or(0.0).max(0.0))
            .collect::<Vec<_>>();
        let pos_y = lines
            .iter()
            .enumerate()
            .map(|(index, _)| {
                Spring::with_parameters(
                    center + (index as f64 - active_idx as f64) * group_gap,
                    POS_STIFFNESS,
                    POS_DAMPING,
                    1.0,
                    false,
                )
            })
            .collect();
        let scale = lines
            .iter()
            .map(|_| Spring::with_parameters(0.97, 100.0, 25.0, 2.0, false))
            .collect();
        let blur = lines
            .iter()
            .map(|_| Spring::with_parameters(0.0, BLUR_STIFFNESS, BLUR_DAMPING, 1.0, true))
            .collect();
        Self {
            pos_y,
            scale,
            blur,
            active_idx,
            focus_idx: active_idx,
            playing_indices: Vec::new(),
            highlighted_indices: Vec::new(),
            previous_t_ms: None,
            transition_ms: 0,
            stagger_delays_ms: vec![0; lines.len()],
            height: height as f64,
            initialized: false,
            group_heights,
            group_gap: group_gap.max(0.0),
            interlude_slot_height: interlude_slot_height.max(0.0),
            interlude: None,
        }
    }

    pub fn update(&mut self, lines: &[LyricLine], t_ms: u64, fps: u32) {
        if lines.is_empty() || fps == 0 {
            return;
        }
        let previous_t_ms = self.previous_t_ms;
        let seeking = previous_t_ms.is_some_and(|previous| t_ms < previous);
        let playing = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| {
                (line.start_ms < line.end_ms && line.start_ms <= t_ms && t_ms < line.end_ms)
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let next_active = playing
            .first()
            .copied()
            .unwrap_or_else(|| active_index(lines, t_ms));
        let next_focus = anticipatory_index(lines, t_ms, next_active);
        let next_interlude = interlude_for(lines, t_ms);
        let started = playing
            .iter()
            .any(|index| !self.playing_indices.contains(index));
        if seeking || !self.initialized {
            self.highlighted_indices = playing.clone();
        } else if next_interlude.is_some() && playing.is_empty() {
            self.highlighted_indices.clear();
        } else if started {
            self.highlighted_indices
                .retain(|index| lines[*index].end_ms > t_ms);
            for index in &playing {
                if !self.highlighted_indices.contains(index) {
                    self.highlighted_indices.push(*index);
                }
            }
        }
        self.playing_indices = playing;
        self.highlighted_indices.sort_unstable();
        if !self.initialized {
            self.active_idx = next_active;
            self.focus_idx = next_focus;
            self.interlude = next_interlude;
            self.set_targets(lines);
            self.initialized = true;
        } else {
            let focus_changed = next_focus != self.focus_idx;
            let interlude_changed = next_interlude != self.interlude;
            self.active_idx = next_active;
            self.focus_idx = next_focus;
            self.interlude = next_interlude;
            if focus_changed || interlude_changed {
                let (stiffness, damping) =
                    position_policy(lines, next_focus, next_interlude.is_some());
                self.transition_ms = t_ms;
                for index in 0..lines.len() {
                    let delay = if next_interlude.is_some() || index <= next_focus {
                        0
                    } else {
                        ((index - next_focus) as f64 * 50.0 / 1.05) as u64
                    };
                    self.stagger_delays_ms[index] = delay;
                    self.pos_y[index].set_parameters(stiffness, damping, 1.0, false);
                    let target_y = self.target_y(index);
                    if delay == 0 {
                        self.pos_y[index].set_target_position(target_y);
                    }
                    self.scale[index].set_target_position(if index == self.focus_idx {
                        1.0
                    } else {
                        0.97
                    });
                }
            }
        }
        let elapsed = t_ms.saturating_sub(self.transition_ms);
        for index in 0..lines.len() {
            let target_blur = self.target_blur(index);
            if seeking {
                self.blur[index].set_position(target_blur);
            } else if elapsed >= self.stagger_delays_ms[index] {
                let target_y = self.target_y(index);
                self.pos_y[index].set_target_position(target_y);
                self.blur[index].set_target_position(target_blur);
            }
        }
        let dt = if seeking {
            0.0
        } else {
            previous_t_ms.map_or(1.0 / fps as f64, |previous| {
                let delta = t_ms.saturating_sub(previous) as f64 / 1000.0;
                if delta > 0.1 { 1.0 / fps as f64 } else { delta }
            })
        };
        for spring in &mut self.pos_y {
            spring.update(dt);
        }
        for spring in &mut self.scale {
            spring.update(dt);
        }
        for spring in &mut self.blur {
            spring.update(dt);
        }
        self.previous_t_ms = Some(t_ms);
    }
    pub fn active_idx(&self) -> usize {
        self.active_idx
    }
    pub fn is_active(&self, index: usize) -> bool {
        self.interlude.is_none() && self.highlighted_indices.binary_search(&index).is_ok()
    }
    pub fn playing_indices(&self) -> &[usize] {
        &self.playing_indices
    }
    pub fn highlighted_indices(&self) -> &[usize] {
        &self.highlighted_indices
    }
    pub fn is_playing(&self, index: usize) -> bool {
        self.playing_indices.binary_search(&index).is_ok()
    }
    pub fn scroll_to_idx(&self) -> usize {
        self.focus_idx
    }
    pub fn latest_highlighted_idx(&self) -> Option<usize> {
        self.highlighted_indices.iter().copied().max()
    }

    pub fn focus_idx(&self) -> usize {
        self.focus_idx
    }

    pub fn save_checkpoint(&self) -> LayoutCheckpoint {
        LayoutCheckpoint {
            schema_version: 1,
            width: 0,
            height: 0,
            fps: 0,
            layout_height: self.height,
            line_count: self.pos_y.len(),
            pos_y: self.pos_y.iter().map(Spring::save_checkpoint).collect(),
            scale: self.scale.iter().map(Spring::save_checkpoint).collect(),
            blur: self.blur.iter().map(Spring::save_checkpoint).collect(),
            active_idx: self.active_idx,
            focus_idx: self.focus_idx,
            playing_indices: self.playing_indices.clone(),
            highlighted_indices: self.highlighted_indices.clone(),
            previous_t_ms: self.previous_t_ms,
            transition_ms: self.transition_ms,
            stagger_delays_ms: self.stagger_delays_ms.clone(),
            group_heights: self.group_heights.clone(),
            group_gap: self.group_gap,
            interlude_slot_height: self.interlude_slot_height,
            initialized: self.initialized,
            interlude: self.interlude,
        }
    }

    pub fn restore_checkpoint(&mut self, checkpoint: &LayoutCheckpoint) -> Result<()> {
        checkpoint.validate_schema()?;
        if checkpoint.line_count != self.pos_y.len()
            || (checkpoint.layout_height - self.height).abs() > 1e-9
        {
            bail!("layout checkpoint dimensions do not match renderer");
        }
        for (spring, saved) in self.pos_y.iter_mut().zip(&checkpoint.pos_y) {
            spring.restore_checkpoint(saved)?;
        }
        for (spring, saved) in self.scale.iter_mut().zip(&checkpoint.scale) {
            spring.restore_checkpoint(saved)?;
        }
        for (spring, saved) in self.blur.iter_mut().zip(&checkpoint.blur) {
            spring.restore_checkpoint(saved)?;
        }
        self.active_idx = checkpoint.active_idx;
        self.focus_idx = checkpoint.focus_idx;
        self.playing_indices = checkpoint.playing_indices.clone();
        self.highlighted_indices = checkpoint.highlighted_indices.clone();
        self.previous_t_ms = checkpoint.previous_t_ms;
        self.transition_ms = checkpoint.transition_ms;
        self.stagger_delays_ms = checkpoint.stagger_delays_ms.clone();
        self.group_heights = checkpoint.group_heights.clone();
        self.group_gap = checkpoint.group_gap;
        self.interlude_slot_height = checkpoint.interlude_slot_height;
        self.initialized = checkpoint.initialized;
        self.interlude = checkpoint.interlude;
        Ok(())
    }

    fn group_height(&self, index: usize) -> f64 {
        self.group_heights.get(index).copied().unwrap_or(0.0)
    }

    fn interlude_slot_after(&self, index: usize) -> f64 {
        (self
            .interlude
            .is_some_and(|interlude| interlude.next_idx == index + 1))
        .then_some(self.interlude_slot_height)
        .unwrap_or(0.0)
    }

    pub fn interlude(&self) -> Option<Interlude> {
        self.interlude
    }

    pub fn interlude_top_y(&self, next_idx: usize) -> Option<f32> {
        let interlude = self.interlude?;
        (interlude.next_idx == next_idx).then_some(
            self.pos_y.get(next_idx)?.current_position() as f32 - self.interlude_slot_height as f32,
        )
    }

    fn target_y(&self, index: usize) -> f64 {
        let focus_height = self.group_height(self.focus_idx);
        let focus_top = self.height * 0.35 - focus_height / 2.0;
        if index < self.focus_idx {
            let preceding = (index..self.focus_idx)
                .map(|item| {
                    self.group_height(item) + self.group_gap + self.interlude_slot_after(item)
                })
                .sum::<f64>();
            focus_top - preceding
        } else if index == self.focus_idx {
            focus_top
        } else {
            let following = (self.focus_idx..index)
                .map(|item| {
                    self.group_height(item) + self.group_gap + self.interlude_slot_after(item)
                })
                .sum::<f64>();
            focus_top + following
        }
    }

    fn target_blur(&self, index: usize) -> f64 {
        let duet_overlap_playing = self.playing_indices.len() > 1
            && self.playing_indices.binary_search(&index).is_ok();
        let distance = if duet_overlap_playing || index == self.focus_idx {
            0
        } else if index < self.focus_idx {
            self.focus_idx - index + 1
        } else {
            let reference_idx = if self.playing_indices.len() > 1 {
                self.playing_indices
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(self.focus_idx)
                    .max(self.focus_idx)
            } else {
                self.focus_idx
            };
            index.saturating_sub(reference_idx)
        };
        distance.min(MAX_BLUR_LEVEL) as f64
    }

    fn set_targets(&mut self, lines: &[LyricLine]) {
        for index in 0..lines.len() {
            let target_y = self.target_y(index);
            let target_blur = self.target_blur(index);
            self.pos_y[index].set_position(target_y);
            self.scale[index].set_position(if index == self.focus_idx { 1.0 } else { 0.97 });
            self.blur[index].set_position(target_blur);
        }
    }
}
fn anticipatory_index(lines: &[LyricLine], t_ms: u64, current: usize) -> usize {
    let Some(next) = lines.get(current + 1) else {
        return current;
    };
    if let Some(interlude) = interlude_between(lines, current + 1) {
        let float_at = interlude
            .end_ms
            .saturating_add(INTERLUDE_FLOAT_DELAY_MS)
            .min(next.start_ms.saturating_sub(1));
        return (t_ms >= float_at).then_some(current + 1).unwrap_or(current);
    }
    let current_start = lines[current].start_ms;
    let interval = next.start_ms.saturating_sub(current_start);
    let anticipation = (interval / 3).clamp(MIN_ANTICIPATION_MS, MAX_ANTICIPATION_MS);
    let scan_end = scan_end_ms(&lines[current]).min(next.start_ms);
    let transition_at = next
        .start_ms
        .saturating_sub(anticipation)
        .max(scan_end)
        .max(current_start);
    if t_ms >= transition_at {
        current + 1
    } else {
        current
    }
}

fn active_index(lines: &[LyricLine], t_ms: u64) -> usize {
    lines
        .iter()
        .position(|line| line.start_ms <= t_ms && t_ms < line.end_ms)
        .unwrap_or_else(|| {
            lines
                .iter()
                .rposition(|line| line.start_ms <= t_ms)
                .unwrap_or(0)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines() -> Vec<LyricLine> {
        (0..3)
            .map(|index| LyricLine {
                start_ms: index * 2_000,
                end_ms: (index + 1) * 2_000,
                text: format!("Line {index}"),
                translation: None,
                agent_id: None,
                is_duet: false,
                is_background: false,
                background_vocal: None,
                words: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn scroll_begins_before_next_line_start_without_early_highlight() {
        let lines = lines();
        let mut layout = Layout::new(&lines, 1_080.0, &[70.0, 70.0, 70.0], 0.0, 0.0);
        layout.update(&lines, 0, 30);
        let initial = layout.pos_y[0].current_position();
        let initial_scale = layout.scale[0].current_position();
        layout.update(&lines, 1_500, 30);
        assert!((layout.pos_y[0].current_position() - initial).abs() < 1e-9);
        assert!((layout.scale[0].current_position() - initial_scale).abs() < 1e-9);
        layout.update(&lines, 1_800, 30);
        assert!(layout.pos_y[0].current_position() < initial);
        assert!(layout.scale[0].current_position() < initial_scale);
        assert!(layout.scale[1].current_position() > 0.97);
        assert_eq!(layout.active_idx(), 0);
        layout.update(&lines, 2_000, 30);
        assert_eq!(layout.active_idx(), 1);
    }
    #[test]
    fn transition_moves_over_multiple_frames() {
        let lines = lines();
        let mut layout = Layout::new(&lines, 1_080.0, &[70.0, 70.0, 70.0], 0.0, 0.0);
        layout.update(&lines, 0, 30);
        let before = layout.pos_y[0].current_position();
        layout.update(&lines, 2_000, 30);
        let at_switch = layout.pos_y[0].current_position();
        layout.update(&lines, 2_000 + 1_000 / 30, 30);
        let next = layout.pos_y[0].current_position();
        assert!(at_switch < before);
        assert!(next < at_switch);
        assert!((before - at_switch).abs() < 20.0);
    }

    #[test]
    fn scale_settles_monotonically_after_line_change() {
        let lines = lines();
        let mut layout = Layout::new(&lines, 1_080.0, &[70.0, 70.0, 70.0], 0.0, 0.0);
        layout.update(&lines, 0, 30);
        layout.update(&lines, 2_000, 30);
        let mut previous = layout.scale[0].current_position();
        for frame in 1..60 {
            layout.update(&lines, 2_000 + frame * 1_000 / 30, 30);
            let current = layout.scale[0].current_position();
            assert!(current <= previous + 1e-9);
            assert!(current >= 0.97 - 1e-9);
            previous = current;
        }
        assert!((previous - 0.97).abs() < 0.01);
    }

    #[test]
    fn variable_group_heights_keep_the_gap_constant() {
        let lines = lines();
        let group_heights = [100.0, 200.0, 100.0];
        let mut layout = Layout::new(&lines, 1_080.0, &group_heights, 20.0, 0.0);
        layout.update(&lines, 0, 30);
        let first = layout.pos_y[0].current_position();
        let second = layout.pos_y[1].current_position();
        let third = layout.pos_y[2].current_position();
        assert!((second - (first + group_heights[0] + 20.0)).abs() < 1e-9);
        assert!((third - (second + group_heights[1] + 20.0)).abs() < 1e-9);
    }

    #[test]
    fn interlude_dots_end_before_next_line_float() {
        let lines = vec![
            LyricLine {
                start_ms: 0,
                end_ms: 1_000,
                text: "First".to_owned(),
                translation: None,
                agent_id: None,
                is_duet: false,
                is_background: false,
                background_vocal: None,
                words: Vec::new(),
            },
            LyricLine {
                start_ms: 6_000,
                end_ms: 7_000,
                text: "Second".to_owned(),
                translation: None,
                agent_id: None,
                is_duet: false,
                is_background: false,
                background_vocal: None,
                words: Vec::new(),
            },
        ];
        let interlude = interlude_for(&lines, 2_000).unwrap();
        assert_eq!(interlude.start_ms, 1_000);
        assert_eq!(interlude.end_ms, 6_000);
        let mut layout = Layout::new(&lines, 1_080.0, &[70.0, 70.0], 0.0, 40.0);
        layout.update(&lines, 2_000, 30);
        assert!(
            (layout.pos_y[1].current_position() - layout.pos_y[0].current_position() - 110.0).abs()
                < 1e-9
        );
        assert!(!layout.is_active(0));
        assert!(!layout.is_active(1));
        layout.update(&lines, 5_800, 30);
        assert_eq!(layout.focus_idx, 0);
        layout.update(&lines, 6_000, 30);
        assert_eq!(layout.focus_idx, 1);
        assert_eq!(layout.active_idx(), 1);
    }

    #[test]
    fn checkpoint_resume_preserves_animated_layout() {
        let lines = lines();
        let mut continuous = Layout::new(&lines, 1_080.0, &[70.0, 70.0, 70.0], 0.0, 40.0);
        let mut resumed = Layout::new(&lines, 1_080.0, &[70.0, 70.0, 70.0], 0.0, 40.0);
        for timestamp in [0, 33, 66, 99, 132, 1_800, 1_833] {
            continuous.update(&lines, timestamp, 30);
        }
        let checkpoint = continuous.save_checkpoint();
        checkpoint.validate_schema().unwrap();
        resumed.restore_checkpoint(&checkpoint).unwrap();
        for timestamp in [1_866, 1_899, 2_000, 2_033, 2_500, 5_000, 6_000] {
            continuous.update(&lines, timestamp, 30);
            resumed.update(&lines, timestamp, 30);
            for (left, right) in continuous.pos_y.iter().zip(&resumed.pos_y) {
                assert_eq!(left.current_position(), right.current_position());
                assert_eq!(left.current_velocity(), right.current_velocity());
            }
            for (left, right) in continuous.blur.iter().zip(&resumed.blur) {
                assert_eq!(left.current_position(), right.current_position());
            }
        }
    }

    #[test]
    fn checkpoint_rejects_future_schema() {
        let lines = lines();
        let mut checkpoint = Layout::new(&lines, 1_080.0, &[70.0, 70.0, 70.0], 0.0, 0.0)
            .save_checkpoint();
        checkpoint.schema_version = 2;
        assert!(checkpoint.validate_schema().is_err());
    }
}

fn position_policy(lines: &[LyricLine], focus_idx: usize, interlude: bool) -> (f64, f64) {
    if interlude || focus_idx == 0 {
        return (90.0, 15.0);
    }
    let interval = lines[focus_idx]
        .start_ms
        .saturating_sub(lines[focus_idx - 1].start_ms)
        .clamp(100, 800) as f64;
    let ratio = (1.0 - (interval - 100.0) / 700.0).powf(0.2);
    let stiffness = 170.0 + ratio * 50.0;
    (stiffness, stiffness.sqrt() * 2.2)
}

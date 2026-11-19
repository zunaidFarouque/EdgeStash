pub mod d2d;

use edgestash_ipc::types::Rect;

use crate::animator::ease_out_cubic;

/// Deterministic opacity interpolator driving smooth backdrop dim fade-in and fade-out.
#[derive(Debug, Clone)]
pub struct DimScrimFade {
    start_alpha: f32,
    current_alpha: f32,
    target_alpha: f32,
    started_at_ms: u64,
    duration_ms: u32,
    animating: bool,
    pub monitor_rect: Rect,
}

impl Default for DimScrimFade {
    fn default() -> Self {
        Self {
            start_alpha: 0.0,
            current_alpha: 0.0,
            target_alpha: 0.0,
            started_at_ms: 0,
            duration_ms: 160,
            animating: false,
            monitor_rect: Rect::default(),
        }
    }
}

impl DimScrimFade {
    /// Starts or reverses a fade toward `target_alpha` (clamped `0.0..=0.6`) over `duration_ms`.
    pub fn set_target(&mut self, target_alpha: f32, duration_ms: u32, now_ms: u64) {
        let clamped_target = target_alpha.clamp(0.0, 0.6);
        if (self.target_alpha - clamped_target).abs() < 0.0005 && !self.animating {
            return;
        }
        // Advance current alpha up to now_ms first so mid-fade reversals start seamlessly
        let _ = self.advance(now_ms);
        self.target_alpha = clamped_target;
        self.duration_ms = duration_ms;
        if duration_ms == 0 || (self.current_alpha - clamped_target).abs() < 0.0005 {
            self.start_alpha = clamped_target;
            self.current_alpha = clamped_target;
            self.animating = false;
        } else {
            self.start_alpha = self.current_alpha;
            self.started_at_ms = now_ms;
            self.animating = true;
        }
    }

    /// Evaluates opacity at `now_ms`, returning `(alpha, still_animating)`.
    pub fn advance(&mut self, now_ms: u64) -> (f32, bool) {
        if !self.animating {
            return (self.current_alpha, false);
        }
        if self.duration_ms == 0 {
            self.current_alpha = self.target_alpha;
            self.animating = false;
            return (self.current_alpha, false);
        }
        let elapsed = now_ms.saturating_sub(self.started_at_ms);
        if elapsed >= u64::from(self.duration_ms) {
            self.current_alpha = self.target_alpha;
            self.animating = false;
            return (self.current_alpha, false);
        }
        let raw_t = (elapsed as f32) / (self.duration_ms as f32);
        let eased = ease_out_cubic(raw_t);
        self.current_alpha = self.start_alpha + (self.target_alpha - self.start_alpha) * eased;
        (self.current_alpha, true)
    }

    pub fn current_alpha(&self) -> f32 {
        self.current_alpha
    }

    pub fn is_visible(&self) -> bool {
        self.current_alpha > 0.001 || self.target_alpha > 0.001 || self.animating
    }
}

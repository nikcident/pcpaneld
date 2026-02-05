use std::collections::VecDeque;
use std::time::Instant;

/// Per-control signal processing pipeline.
///
/// Stages: rolling average -> delta threshold -> debounce.
/// Endpoints (0, 255) always pass through regardless of threshold.
#[derive(Debug)]
pub struct SignalPipeline {
    /// Rolling average window.
    window: VecDeque<u8>,
    window_size: usize,

    /// Delta threshold: suppress output if change from last emitted value
    /// is less than this.
    delta_threshold: u8,

    /// Debounce: suppress output if less than this duration since last emit.
    debounce_ms: u64,

    /// Last value that passed through the pipeline.
    last_emitted: Option<u8>,

    /// Timestamp of last emission.
    last_emit_time: Option<Instant>,
}

impl SignalPipeline {
    /// Create a pipeline with custom parameters.
    pub fn new(window_size: usize, delta_threshold: u8, debounce_ms: u64) -> Self {
        let window_size = window_size.max(1);
        SignalPipeline {
            window: VecDeque::with_capacity(window_size),
            window_size,
            delta_threshold,
            debounce_ms,
            last_emitted: None,
            last_emit_time: None,
        }
    }

    /// Create a pipeline with default slider parameters.
    #[cfg(test)]
    pub fn slider_defaults() -> Self {
        Self::new(5, 2, 10)
    }

    /// Create a pipeline with default knob parameters.
    #[cfg(test)]
    pub fn knob_defaults() -> Self {
        Self::new(3, 1, 0)
    }

    /// Process a raw hardware value through the pipeline.
    ///
    /// Returns `Some(value)` if the value should be emitted, `None` if suppressed.
    pub fn process(&mut self, raw: u8) -> Option<u8> {
        self.process_at(raw, Instant::now())
    }

    /// Process with an explicit timestamp (for testing).
    pub fn process_at(&mut self, raw: u8, now: Instant) -> Option<u8> {
        // Endpoints (raw 0 or 255) always pass through immediately,
        // bypassing rolling average, delta threshold, and debounce.
        // This ensures the user can always reach the extremes.
        if raw == 0 || raw == 255 {
            self.window.clear();
            self.window.push_back(raw);
            self.last_emitted = Some(raw);
            self.last_emit_time = Some(now);
            return Some(raw);
        }

        // Stage 1: Rolling average
        self.window.push_back(raw);
        if self.window.len() > self.window_size {
            self.window.pop_front();
        }

        let sum: u32 = self.window.iter().map(|&v| u32::from(v)).sum();
        let avg = (sum / self.window.len() as u32) as u8;

        // Stage 2: Delta threshold
        if let Some(last) = self.last_emitted {
            let delta = (i16::from(avg) - i16::from(last)).unsigned_abs() as u8;
            if delta < self.delta_threshold {
                return None;
            }
        }

        // Stage 3: Debounce
        if self.debounce_ms > 0 {
            if let Some(last_time) = self.last_emit_time {
                let elapsed = now.duration_since(last_time).as_millis() as u64;
                if elapsed < self.debounce_ms {
                    return None;
                }
            }
        }

        self.last_emitted = Some(avg);
        self.last_emit_time = Some(now);
        Some(avg)
    }

    /// Reset the pipeline state. Call when a control is reconnected or reconfigured.
    pub fn reset(&mut self) {
        self.window.clear();
        self.last_emitted = None;
        self.last_emit_time = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn first_value_always_passes() {
        let mut p = SignalPipeline::new(3, 5, 0);
        assert_eq!(p.process(128), Some(128));
    }

    #[test]
    fn rolling_average_smooths_jitter() {
        let mut p = SignalPipeline::new(5, 0, 0);
        // Feed values jittering around 128 (±3)
        let inputs = [125, 131, 126, 130, 128];
        let mut outputs = Vec::new();
        for &v in &inputs {
            if let Some(out) = p.process(v) {
                outputs.push(out);
            }
        }
        // With window=5, after all values are in, the average should be near 128
        let last = *outputs.last().unwrap();
        assert!(
            (126..=130).contains(&last),
            "smoothed output {last} should be near 128"
        );
    }

    #[test]
    fn delta_threshold_suppresses_small_changes() {
        let mut p = SignalPipeline::new(1, 3, 0);
        assert_eq!(p.process(100), Some(100));
        // Change of 1 with threshold 3 -> suppressed
        assert_eq!(p.process(101), None);
        assert_eq!(p.process(102), None);
        // Change of 3 -> passes
        assert_eq!(p.process(103), Some(103));
    }

    #[test]
    fn endpoint_zero_always_passes() {
        let mut p = SignalPipeline::new(1, 10, 100);
        let now = Instant::now();
        assert_eq!(p.process_at(128, now), Some(128));
        // Even with threshold=10 and debounce=100ms, endpoint 0 passes through
        assert_eq!(p.process_at(0, now), Some(0));
    }

    #[test]
    fn endpoint_255_always_passes() {
        let mut p = SignalPipeline::new(1, 10, 100);
        let now = Instant::now();
        assert_eq!(p.process_at(128, now), Some(128));
        assert_eq!(p.process_at(255, now), Some(255));
    }

    #[test]
    fn debounce_suppresses_rapid_changes() {
        let mut p = SignalPipeline::new(1, 0, 10);
        let t0 = Instant::now();

        assert_eq!(p.process_at(100, t0), Some(100));
        // 5ms later, within debounce window
        assert_eq!(p.process_at(110, t0 + Duration::from_millis(5)), None);
        // 15ms later, outside debounce window
        assert_eq!(p.process_at(110, t0 + Duration::from_millis(15)), Some(110));
    }

    #[test]
    fn debounce_zero_means_no_debouncing() {
        let mut p = SignalPipeline::new(1, 0, 0);
        let now = Instant::now();
        assert_eq!(p.process_at(100, now), Some(100));
        assert_eq!(p.process_at(110, now), Some(110));
        assert_eq!(p.process_at(120, now), Some(120));
    }

    #[test]
    fn full_slider_sweep_is_monotonic_and_reaches_endpoints() {
        let mut pipeline = SignalPipeline::slider_defaults();
        let mut outputs = Vec::new();
        for hw_value in 0..=255u8 {
            if let Some(val) = pipeline.process(hw_value) {
                outputs.push(val);
            }
        }
        assert_eq!(*outputs.first().unwrap(), 0, "sweep must start at 0");
        assert_eq!(*outputs.last().unwrap(), 255, "sweep must reach 255");
        assert!(
            outputs.windows(2).all(|w| w[1] >= w[0]),
            "sweep must be monotonically increasing: {outputs:?}"
        );
    }

    #[test]
    fn full_knob_sweep_is_monotonic_and_reaches_endpoints() {
        let mut pipeline = SignalPipeline::knob_defaults();
        let mut outputs = Vec::new();
        for hw_value in 0..=255u8 {
            if let Some(val) = pipeline.process(hw_value) {
                outputs.push(val);
            }
        }
        assert_eq!(*outputs.first().unwrap(), 0);
        assert_eq!(*outputs.last().unwrap(), 255);
        assert!(outputs.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn reverse_sweep_is_monotonically_decreasing() {
        let mut pipeline = SignalPipeline::slider_defaults();
        let mut outputs = Vec::new();
        for hw_value in (0..=255u8).rev() {
            if let Some(val) = pipeline.process(hw_value) {
                outputs.push(val);
            }
        }
        assert_eq!(*outputs.first().unwrap(), 255);
        assert_eq!(*outputs.last().unwrap(), 0);
        assert!(
            outputs.windows(2).all(|w| w[1] <= w[0]),
            "reverse sweep must be monotonically decreasing"
        );
    }

    #[test]
    fn reset_clears_state() {
        let mut p = SignalPipeline::new(3, 5, 0);
        assert_eq!(p.process(128), Some(128));
        p.reset();
        // After reset, first value passes again
        assert_eq!(p.process(130), Some(130));
    }

    #[test]
    fn window_size_one_is_passthrough() {
        let mut p = SignalPipeline::new(1, 0, 0);
        for v in [0, 50, 100, 150, 200, 255] {
            assert_eq!(p.process(v), Some(v));
        }
    }

    #[test]
    fn window_size_zero_clamped_to_one() {
        let mut p = SignalPipeline::new(0, 0, 0);
        assert_eq!(p.process(42), Some(42));
    }

    #[test]
    fn stable_input_doesnt_emit_duplicates() {
        let mut p = SignalPipeline::new(1, 1, 0);
        assert_eq!(p.process(128), Some(128));
        // Delta of 0 with threshold 1 -> suppressed
        assert_eq!(p.process(128), None);
        assert_eq!(p.process(128), None);
    }

    #[test]
    fn endpoint_passes_through_with_rolling_average() {
        // With window=3, we need consecutive 0s to get average to 0
        let mut p = SignalPipeline::new(3, 10, 0);
        // First feed some middle value
        p.process(128);
        // Then go to 0 rapidly
        p.process(0);
        p.process(0);
        let result = p.process(0);
        assert_eq!(result, Some(0));
    }
}

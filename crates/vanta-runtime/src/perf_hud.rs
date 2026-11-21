//! Built-in live Performance HUD (`F8` toggle).
//!
//! Computes rolling 1-second frame pacing statistics — FPS, 1% Low FPS,
//! 0.1% Low FPS, average frametime, and P99 frametime — directly from the
//! lock-free `vkQueuePresentKHR` interval ring buffer in
//! [`crate::android::frame_pacing`].
//!
//! Zero per-frame overhead: `record_present()` is untouched, and HUD snapshots
//! are sampled at most every 350 ms on the GTK pump thread only while enabled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Rolling measurement window: 1 second in microseconds.
pub const WINDOW_US: u64 = 1_000_000;

/// Minimum interval between HUD UI refreshes.
const REFRESH_INTERVAL: Duration = Duration::from_millis(350);

/// Linux evdev code for F8 (`KEY_F8`).
pub const DEFAULT_KEY: i32 = 66;

/// Supported hotkey names for `VANTA_PERF_HUD_KEY`, matching `recording.rs`.
const KEY_NAMES: &[(&str, i32)] = &[
    ("f1", 59),
    ("f2", 60),
    ("f3", 61),
    ("f4", 62),
    ("f5", 63),
    ("f6", 64),
    ("f7", 65),
    ("f8", 66),
    ("f9", 67),
    ("f10", 68),
    ("f12", 88),
    ("scrolllock", 70),
    ("pause", 119),
];

/// Rolling frame-pacing snapshot over recent present intervals.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerfStats {
    pub fps: f64,
    pub low_1_fps: f64,
    pub low_01_fps: f64,
    pub avg_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
    pub samples: usize,
}

impl PerfStats {
    /// Compute stats from an arbitrary slice of frame intervals in microseconds.
    ///
    /// Returns `None` if there are no positive intervals.
    pub fn compute(intervals_us: &[u32]) -> Option<Self> {
        let mut buf: Vec<u32> = intervals_us.iter().copied().filter(|&x| x > 0).collect();
        Self::compute_in_place(&mut buf)
    }

    /// Compute stats in place, reusing `buf` to avoid heap allocations on each
    /// periodic HUD tick. Zero intervals are stripped in place.
    pub fn compute_in_place(buf: &mut Vec<u32>) -> Option<Self> {
        buf.retain(|&x| x > 0);
        let n = buf.len();
        if n == 0 {
            return None;
        }
        let sum_us: u64 = buf.iter().map(|&x| x as u64).sum();
        let avg_us = sum_us as f64 / n as f64;
        let avg_ms = avg_us / 1000.0;
        let fps = if avg_ms > 0.0 { 1000.0 / avg_ms } else { 0.0 };

        buf.sort_unstable();
        let p99_us = percentile_sorted(buf, 0.99);
        let p999_us = percentile_sorted(buf, 0.999);

        let p99_ms = p99_us as f64 / 1000.0;
        let p999_ms = p999_us as f64 / 1000.0;
        let low_1_fps = if p99_ms > 0.0 { 1000.0 / p99_ms } else { 0.0 };
        let low_01_fps = if p999_ms > 0.0 { 1000.0 / p999_ms } else { 0.0 };

        Some(Self {
            fps,
            low_1_fps,
            low_01_fps,
            avg_ms,
            p99_ms,
            p999_ms,
            samples: n,
        })
    }

    /// Compact monospace pill text for the header bar (e.g. `312 FPS · 1% 248 · 3.2ms`).
    pub fn format_badge(&self) -> String {
        format!(
            "{:.0} FPS \u{00b7} 1% {:.0} \u{00b7} {:.1}ms",
            self.fps, self.low_1_fps, self.avg_ms
        )
    }

    /// Detailed breakdown shown in the pill tooltip.
    pub fn format_tooltip(&self) -> String {
        format!(
            "FPS: {:.1}\n1% Low: {:.1} FPS (P99 {:.2} ms)\n0.1% Low: {:.1} FPS (P99.9 {:.2} ms)\nAvg Frametime: {:.2} ms ({} frames)",
            self.fps,
            self.low_1_fps,
            self.p99_ms,
            self.low_01_fps,
            self.p999_ms,
            self.avg_ms,
            self.samples,
        )
    }

    /// Convert to the `vanta-protocol` wire snapshot type for `state.get`.
    pub fn to_protocol_snapshot(&self) -> vanta_protocol::msg::PerfStatsSnapshot {
        let to_x10 = |v: f64| -> u32 { (v * 10.0).round().clamp(0.0, u32::MAX as f64) as u32 };
        let to_us = |ms: f64| -> u32 { (ms * 1000.0).round().clamp(0.0, u32::MAX as f64) as u32 };
        vanta_protocol::msg::PerfStatsSnapshot {
            fps_x10: to_x10(self.fps),
            low_1_fps_x10: to_x10(self.low_1_fps),
            low_01_fps_x10: to_x10(self.low_01_fps),
            avg_frametime_us: to_us(self.avg_ms),
            p99_frametime_us: to_us(self.p99_ms),
            samples: self.samples.min(u32::MAX as usize) as u32,
        }
    }
}

/// Nearest-rank upper percentile on a non-empty sorted slice of microsecond intervals.
///
/// Uses `ceil((n - 1) * q)` so a single hitch spike in 100 frames is captured at
/// p99 (1% worst) and a single spike in 1000 frames is captured at p99.9 (0.1%
/// worst) without polluting p99.
fn percentile_sorted(sorted: &[u32], q: f64) -> u32 {
    debug_assert!(!sorted.is_empty());
    let last = sorted.len() - 1;
    let idx = ((last as f64) * q).ceil() as usize;
    sorted[idx.min(last)]
}

// ------------------------------------------------------------------ state/env

fn enabled_cell() -> &'static AtomicBool {
    static ON: OnceLock<AtomicBool> = OnceLock::new();
    ON.get_or_init(|| AtomicBool::new(enabled_for(std::env::var("VANTA_PERF_HUD").ok().as_deref())))
}

/// Pure env-var parser, separated for unit testing without mutating global env.
pub fn enabled_for(v: Option<&str>) -> bool {
    matches!(
        v.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// Whether the Performance HUD is currently enabled.
pub fn enabled() -> bool {
    enabled_cell().load(Ordering::Relaxed)
}

/// Set whether the Performance HUD is enabled (called from `live_settings` or hotkey).
pub fn set_enabled(on: bool) {
    enabled_cell().store(on, Ordering::Release);
}

/// Toggle the Performance HUD and return the new state.
pub fn toggle() -> bool {
    let cell = enabled_cell();
    let prev = cell.fetch_xor(true, Ordering::AcqRel);
    !prev
}

// --------------------------------------------------------------------- hotkey

/// Parse `VANTA_PERF_HUD_KEY`: default F8 (66), `off`/`none`/`0` disables it.
pub fn parse_key(value: Option<&str>) -> Result<Option<i32>, String> {
    let v = match value.map(str::trim) {
        None | Some("") => return Ok(Some(DEFAULT_KEY)),
        Some(v) => v.to_ascii_lowercase().replace(['_', '-', ' '], ""),
    };
    if matches!(v.as_str(), "off" | "none" | "0" | "no" | "disabled") {
        return Ok(None);
    }
    KEY_NAMES
        .iter()
        .find(|(n, _)| *n == v)
        .map(|(_, code)| Some(*code))
        .ok_or_else(|| {
            let names: Vec<&str> = KEY_NAMES.iter().map(|(n, _)| *n).collect();
            format!("VANTA_PERF_HUD_KEY={value:?} is not off or one of {}", names.join(", "))
        })
}

fn hotkey() -> Option<i32> {
    static KEY: OnceLock<Option<i32>> = OnceLock::new();
    *KEY.get_or_init(|| {
        let var = std::env::var("VANTA_PERF_HUD_KEY").ok();
        match parse_key(var.as_deref()) {
            Ok(Some(code)) => Some(code),
            Ok(None) => None,
            Err(e) => {
                println!("[perf-hud] hotkey: off, {e}");
                None
            }
        }
    })
}

static HOTKEY_HELD: AtomicBool = AtomicBool::new(false);

/// Offer a key event to the Performance HUD. Returns `true` when it was the
/// toggle hotkey (F8 by default) and must be consumed before reaching the engine.
///
/// Matches `recording::hotkey_event`: chords with Ctrl or Alt are left alone,
/// and auto-repeats of a held key are consumed without re-toggling.
pub fn hotkey_event(evdev: i32, down: bool, modified: bool) -> bool {
    hotkey_event_with_code(hotkey(), evdev, down, modified)
}

fn hotkey_event_with_code(code: Option<i32>, evdev: i32, down: bool, modified: bool) -> bool {
    let Some(code) = code else { return false };
    if evdev != code {
        return false;
    }
    if down {
        if modified && !HOTKEY_HELD.load(Ordering::Acquire) {
            return false;
        }
        if !HOTKEY_HELD.swap(true, Ordering::AcqRel) {
            let now_on = toggle();
            println!("[perf-hud] {}", if now_on { "on" } else { "off" });
        }
        true
    } else {
        HOTKEY_HELD.swap(false, Ordering::AcqRel)
    }
}

// ------------------------------------------------------------------ HUD pump

/// A pending change for the window's Performance HUD badge.
#[derive(Debug, Clone)]
pub enum HudUpdate {
    Show {
        badge: String,
        tooltip: String,
        stats: Option<PerfStats>,
    },
    Hide,
}

struct PumpState {
    visible: bool,
    last_refresh: Option<Instant>,
    scratch: Vec<u32>,
}

impl PumpState {
    const fn new() -> Self {
        Self { visible: false, last_refresh: None, scratch: Vec::new() }
    }
}

static PUMP: Mutex<PumpState> = Mutex::new(PumpState::new());
/// Fast lock-free mirror of `PUMP.visible` so the disabled steady state costs
/// one relaxed atomic load per pump tick.
static SHOWN: AtomicBool = AtomicBool::new(false);

/// Poll for a Performance HUD refresh. Called from the Wayland GTK pump tick.
///
/// Costs a single relaxed atomic load when disabled and already hidden, and
/// computes a rolling 1-second snapshot at most once every 350 ms while enabled.
pub fn poll_hud_update() -> Option<HudUpdate> {
    let on = enabled();
    if !on && !SHOWN.load(Ordering::Relaxed) {
        return None;
    }
    let mut state = PUMP.lock().unwrap_or_else(|e| e.into_inner());
    if !on {
        if state.visible {
            state.visible = false;
            state.last_refresh = None;
            SHOWN.store(false, Ordering::Release);
            return Some(HudUpdate::Hide);
        }
        return None;
    }
    let now = Instant::now();
    let due = !state.visible
        || state.last_refresh.is_none_or(|t| now.duration_since(t) >= REFRESH_INTERVAL);
    if !due {
        return None;
    }
    state.visible = true;
    state.last_refresh = Some(now);
    SHOWN.store(true, Ordering::Release);

    crate::android::frame_pacing::snapshot_window_us(WINDOW_US, &mut state.scratch);
    let stats = PerfStats::compute_in_place(&mut state.scratch);
    let (badge, tooltip) = match stats {
        Some(s) => (s.format_badge(), s.format_tooltip()),
        None => (
            "-- FPS".to_string(),
            "Performance HUD active — waiting for frames".to_string(),
        ),
    };
    Some(HudUpdate::Show { badge, tooltip, stats })
}

/// Serialise tests across modules that mutate the global HUD state.
#[cfg(test)]
pub(crate) static TEST_GUARD: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_zero_intervals_return_none() {
        assert_eq!(PerfStats::compute(&[]), None);
        assert_eq!(PerfStats::compute(&[0, 0, 0]), None);
    }

    #[test]
    fn steady_60fps_frames_yield_exact_stats() {
        // 60 frames at exactly 16,667 us (~60.0 FPS)
        let intervals = vec![16_667u32; 60];
        let stats = PerfStats::compute(&intervals).unwrap();
        assert_eq!(stats.samples, 60);
        assert!((stats.avg_ms - 16.667).abs() < 1e-6);
        assert!((stats.p99_ms - 16.667).abs() < 1e-6);
        assert!((stats.p999_ms - 16.667).abs() < 1e-6);
        assert!((stats.fps - 59.9988).abs() < 0.01);
        assert!((stats.low_1_fps - 59.9988).abs() < 0.01);
        assert!((stats.low_01_fps - 59.9988).abs() < 0.01);
        assert_eq!(stats.format_badge(), "60 FPS \u{00b7} 1% 60 \u{00b7} 16.7ms");
    }

    #[test]
    fn single_hitch_spike_in_100_frames_shows_in_1_percent_and_01_percent_lows() {
        // 99 frames at 10 ms (100 FPS) + 1 hitch spike at 50 ms (20 FPS)
        let mut intervals = vec![10_000u32; 99];
        intervals.push(50_000);
        let stats = PerfStats::compute(&intervals).unwrap();
        assert_eq!(stats.samples, 100);
        // Average: (99*10 + 50)/100 = 10.4 ms -> ~96.15 FPS
        assert!((stats.avg_ms - 10.4).abs() < 1e-6);
        assert!((stats.fps - (1000.0 / 10.4)).abs() < 1e-3);
        // Both p99 and p99.9 catch the 1-in-100 spike (50 ms -> 20 FPS)
        assert!((stats.p99_ms - 50.0).abs() < 1e-6);
        assert!((stats.low_1_fps - 20.0).abs() < 1e-6);
        assert!((stats.low_01_fps - 20.0).abs() < 1e-6);
    }

    #[test]
    fn single_hitch_spike_in_1000_frames_hits_01_percent_low_but_not_1_percent_low() {
        // 999 frames at 4 ms (250 FPS) + 1 hitch spike at 40 ms (25 FPS)
        let mut intervals = vec![4_000u32; 999];
        intervals.push(40_000);
        let stats = PerfStats::compute(&intervals).unwrap();
        assert_eq!(stats.samples, 1000);
        // 1% low is still 250 FPS (only 0.1% of frames hitched)
        assert!((stats.p99_ms - 4.0).abs() < 1e-6);
        assert!((stats.low_1_fps - 250.0).abs() < 1e-6);
        // 0.1% low captures the 40 ms spike (25 FPS)
        assert!((stats.p999_ms - 40.0).abs() < 1e-6);
        assert!((stats.low_01_fps - 25.0).abs() < 1e-6);
    }

    #[test]
    fn protocol_snapshot_preserves_values_in_fixed_point() {
        let stats = PerfStats::compute(&vec![5_000u32; 50]).unwrap();
        let snap = stats.to_protocol_snapshot();
        assert_eq!(snap.fps_x10, 2000);
        assert_eq!(snap.low_1_fps_x10, 2000);
        assert_eq!(snap.low_01_fps_x10, 2000);
        assert_eq!(snap.avg_frametime_us, 5000);
        assert_eq!(snap.p99_frametime_us, 5000);
        assert_eq!(snap.samples, 50);
    }

    #[test]
    fn env_parsing_defaults_off_and_accepts_truthy_words() {
        assert!(!enabled_for(None));
        assert!(!enabled_for(Some("")));
        assert!(!enabled_for(Some("0")));
        assert!(!enabled_for(Some("false")));
        assert!(enabled_for(Some("1")));
        assert!(enabled_for(Some("true")));
        assert!(enabled_for(Some("YES")));
        assert!(enabled_for(Some("on")));
    }

    #[test]
    fn hotkey_parsing_defaults_to_f8_and_accepts_off() {
        assert_eq!(parse_key(None), Ok(Some(DEFAULT_KEY)));
        assert_eq!(parse_key(Some("F8")), Ok(Some(66)));
        assert_eq!(parse_key(Some("off")), Ok(None));
        assert!(parse_key(Some("banana")).is_err());
    }

    #[test]
    fn f8_hotkey_toggles_once_per_press_and_ignores_chords() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let before = enabled();
        HOTKEY_HELD.store(false, Ordering::SeqCst);
        set_enabled(false);

        // Ctrl+F8 passes through untouched
        assert!(!hotkey_event_with_code(Some(DEFAULT_KEY), DEFAULT_KEY, true, true));
        assert!(!enabled());

        // Plain F8 down toggles on; repeat down is consumed without re-toggling
        assert!(hotkey_event_with_code(Some(DEFAULT_KEY), DEFAULT_KEY, true, false));
        assert!(enabled());
        assert!(hotkey_event_with_code(Some(DEFAULT_KEY), DEFAULT_KEY, true, false));
        assert!(enabled());

        // Release is consumed
        assert!(hotkey_event_with_code(Some(DEFAULT_KEY), DEFAULT_KEY, false, false));
        assert!(enabled());

        // Second press toggles off
        assert!(hotkey_event_with_code(Some(DEFAULT_KEY), DEFAULT_KEY, true, false));
        assert!(!enabled());
        assert!(hotkey_event_with_code(Some(DEFAULT_KEY), DEFAULT_KEY, false, false));

        // Unrelated key passes through
        assert!(!hotkey_event_with_code(Some(DEFAULT_KEY), 67, true, false));

        set_enabled(before);
    }

    #[test]
    fn poll_hud_update_transitions_show_and_hide() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let before = enabled();
        set_enabled(false);
        // Drain any prior visible state
        let _ = poll_hud_update();
        assert!(poll_hud_update().is_none());

        set_enabled(true);
        assert!(matches!(poll_hud_update(), Some(HudUpdate::Show { .. })));
        // Immediate second poll within 350ms is throttled
        assert!(poll_hud_update().is_none());

        set_enabled(false);
        assert!(matches!(poll_hud_update(), Some(HudUpdate::Hide)));
        // Subsequent poll while off costs no work and returns None
        assert!(poll_hud_update().is_none());

        set_enabled(before);
    }
}

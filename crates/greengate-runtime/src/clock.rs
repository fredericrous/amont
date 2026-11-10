//! The one test-only knob on amont's own waits.
//!
//! Some behaviour exists only after a wait: a quiet run names its log once a
//! check has run for five seconds, `amont land` re-reads the remote after a
//! pause, a tree gate gets `amont.treeLintSlack` after the commit's checks.
//! A test of that behaviour has to outlast the wait, and at full length those
//! tests were most of the integration suite's time.
//!
//! In a `test-hooks` build (every `cargo test` build of the binary, and no
//! `cargo build`) `AMONT_TEST_CLOCK_SCALE`, a factor in (0, 1], shortens
//! each wait that goes through [`scaled`]; `.cargo/config.toml` pins it for
//! the tests, and the fixtures scale their own sleeps by the same factor. In
//! a shipped binary [`scaled`] is the identity and reads nothing. The budgets
//! that kill a check (`amont.timeout`, `amont.idleTimeout`) are not scaled:
//! `tests/timing.rs` proves them on the real clock.

use std::time::Duration;

/// `d`, shortened by the test clock scale where one applies.
pub fn scaled(d: Duration) -> Duration {
    #[cfg(feature = "test-hooks")]
    if let Some(f) = factor() {
        return d.mul_f64(f);
    }
    d
}

#[cfg(feature = "test-hooks")]
fn factor() -> Option<f64> {
    static FACTOR: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    *FACTOR.get_or_init(|| {
        std::env::var("AMONT_TEST_CLOCK_SCALE")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|f| *f > 0.0 && *f <= 1.0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pin in `.cargo/config.toml` reaches the tests, and halves nothing
    /// it should not: a factor, never an offset.
    #[test]
    fn the_scale_is_a_factor() {
        let d = Duration::from_secs(10);
        #[cfg(feature = "test-hooks")]
        assert!(scaled(d) <= d);
        #[cfg(not(feature = "test-hooks"))]
        assert_eq!(scaled(d), d);
        assert_eq!(scaled(Duration::ZERO), Duration::ZERO);
    }
}

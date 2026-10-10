//! The host's load, and how far it stretches a silence budget (ADR-0009).
//!
//! A machine running several worktrees and agents at once makes every tool
//! slower, and a tool that is slow because it is waiting for a core is not
//! stuck. The silence budget is therefore multiplied by how oversubscribed
//! the host is — the one-minute load average over the core count — capped
//! by `amont.idleLoadScale`. A false kill costs a parked commit; a slower
//! verdict on a genuine hang costs minutes.
//!
//! The arithmetic is a calculation with no clock or syscall in it; the
//! reading is one `getloadavg` on Linux and macOS, and a factor of one
//! everywhere else, which the messages say rather than claim a load they
//! never measured.

/// What was read from the host, in thousandths so it travels as atomics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Load {
    /// The one-minute load average × 1000.
    pub avg1_milli: u32,
    /// Logical cores the process may run on.
    pub cores: u32,
}

impl Load {
    /// Oversubscription × 1000: `avg1 / cores`, floored at 1.0 and capped at
    /// `cap`. A host with headroom stretches nothing.
    pub fn factor_milli(self, cap: u64) -> u32 {
        let cores = u64::from(self.cores.max(1));
        let raw = u64::from(self.avg1_milli) / cores;
        let capped = raw.clamp(1000, cap.max(1).saturating_mul(1000));
        u32::try_from(capped).unwrap_or(u32::MAX)
    }

    /// `31.2`, for a message.
    pub fn avg1_text(self) -> String {
        format!(
            "{}.{}",
            self.avg1_milli / 1000,
            (self.avg1_milli % 1000) / 100
        )
    }
}

/// `×3.9`, from a factor in thousandths.
pub fn factor_text(milli: u32) -> String {
    format!("×{}.{}", milli / 1000, (milli % 1000) / 100)
}

/// The silence budget under load: `idle × clamp(load1 / cores, 1, cap)`,
/// rounded to whole seconds, then no more than the ceiling when there is
/// one. Pure, so it is tested to the second.
pub fn scaled_budget(idle: u64, load: Load, cap: u64, ceiling: Option<u64>) -> u64 {
    let factor = u64::from(load.factor_milli(cap));
    let scaled = (idle.saturating_mul(factor) + 500) / 1000;
    match ceiling {
        Some(c) if c > 0 => scaled.min(c),
        _ => scaled,
    }
}

/// The host right now, where it can be read.
pub fn read() -> Option<Load> {
    let avg1 = platform::avg1()?;
    let cores = std::thread::available_parallelism()
        .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
        .unwrap_or(1);
    Some(Load {
        avg1_milli: u32::try_from((avg1 * 1000.0).round() as i64).unwrap_or(u32::MAX),
        cores,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    // libc, which std already links.
    extern "C" {
        #[link_name = "getloadavg"]
        fn getloadavg_raw(loadavg: *mut f64, nelem: i32) -> i32;
    }

    pub fn avg1() -> Option<f64> {
        let mut avg = [0f64; 3];
        // SAFETY: a three-element buffer we own, and we ask for three.
        let n = unsafe { getloadavg_raw(avg.as_mut_ptr(), 3) };
        (n >= 1 && avg[0].is_finite() && avg[0] >= 0.0).then_some(avg[0])
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    pub fn avg1() -> Option<f64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(avg1: f64, cores: u32) -> Load {
        Load {
            avg1_milli: (avg1 * 1000.0) as u32,
            cores,
        }
    }

    /// Headroom stretches nothing; 3.9 over 8 cores is under one; 31.2 over
    /// 8 is ×3.9; 40 over 8 is capped at ×4; a cap of 1 disables the
    /// stretch; the ceiling clamps the result.
    #[test]
    fn the_budget_scales_with_oversubscription_up_to_the_cap() {
        assert_eq!(scaled_budget(120, load(1.0, 8), 4, Some(3600)), 120);
        assert_eq!(scaled_budget(120, load(3.9, 8), 4, Some(3600)), 120);
        assert_eq!(scaled_budget(120, load(16.0, 8), 4, Some(3600)), 240);
        assert_eq!(scaled_budget(120, load(31.2, 8), 4, Some(3600)), 468);
        assert_eq!(scaled_budget(120, load(40.0, 8), 4, Some(3600)), 480);
        assert_eq!(scaled_budget(120, load(31.2, 8), 1, Some(3600)), 120);
        assert_eq!(scaled_budget(120, load(80.0, 8), 16, Some(3600)), 1200);
        assert_eq!(scaled_budget(120, load(31.2, 8), 4, Some(300)), 300);
        assert_eq!(scaled_budget(120, load(40.0, 8), 4, None), 480);
        // Zero cores cannot divide by zero.
        assert_eq!(scaled_budget(120, load(2.0, 0), 4, None), 240);
    }

    #[test]
    fn factors_and_averages_read_as_one_decimal() {
        assert_eq!(load(31.2, 8).factor_milli(4), 3900);
        assert_eq!(factor_text(3900), "×3.9");
        assert_eq!(factor_text(1000), "×1.0");
        assert_eq!(load(31.25, 8).avg1_text(), "31.2");
    }

    /// Where the platform reads a load at all, it is a finite non-negative
    /// number with a core count behind it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn the_platform_reads_a_sane_load() {
        let l = read().expect("linux and macos read the load");
        assert!(l.cores >= 1);
        assert!(l.avg1_milli < 100_000_000);
    }
}

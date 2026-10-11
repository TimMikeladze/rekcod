//! Balloon policy: give unused guest memory back to macOS (EXPERIMENTAL, opt-in
//! with `REKCOD_BALLOON=1`).
//!
//! Guest RAM is host memory the moment the guest touches it, and Virtualization
//! .framework has no free-page reporting. The policy keeps about `headroom`
//! free in the guest: it grows the guest back to full size as soon as free
//! memory runs low and shrinks only after free memory stayed high for a while,
//! so builds aren't slowed down. The page cache (the fast file-sharing cache)
//! is kept unless macOS itself is under memory pressure.
//!
//! Measured on macOS 27 (docs/phase2.md): the guest inflates the balloon as
//! asked, but the VM process's physical footprint does not drop, so this is off
//! by default until the host side actually releases ballooned pages.

const MIB: u64 = 1 << 20;
/// Smallest worthwhile balloon move.
const STEP: u64 = 64 * MIB;
/// Extra free memory tolerated before shrinking.
const SLACK: u64 = 256 * MIB;

/// Headroom kept free in the guest for allocation bursts.
pub fn headroom(max: u64) -> u64 {
    (max / 8).clamp(512 * MIB, 2048 * MIB)
}

/// Seconds free memory must stay above the band before the balloon shrinks.
pub const SHRINK_AFTER_TICKS: u32 = 15;

/// New balloon target (bytes the guest may use), or `None` to leave it.
/// `idle_ticks`: consecutive 1 s ticks with free memory above the band.
pub fn next_target(current: u64, max: u64, min: u64, free: u64, headroom: u64, idle_ticks: u32) -> Option<u64> {
    let round = |b: u64| b / MIB * MIB;
    if free < headroom / 2 {
        // Running low: give everything back at once; a build shouldn't wait on us.
        (current < max).then_some(max)
    } else if free > headroom + SLACK && idle_ticks >= SHRINK_AFTER_TICKS {
        let target = round(current.saturating_sub(free - headroom).max(min));
        (current.saturating_sub(target) >= STEP).then_some(target)
    } else {
        None
    }
}

/// `meminfo` reply (KiB) -> (free, cached) in bytes.
pub fn parse_meminfo(reply: &str) -> Option<(u64, u64)> {
    let v: Vec<u64> = reply.split_whitespace().map(|x| x.parse().ok()).collect::<Option<_>>()?;
    (v.len() == 4).then(|| (v[1] * 1024, v[3] * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;
    const GIB: u64 = 1 << 30;

    const IDLE: u32 = SHRINK_AFTER_TICKS;

    #[test]
    fn returns_free_memory_beyond_headroom_only_after_sustained_idle() {
        // 8 GiB VM, 6 GiB free after a build, 1 GiB headroom: guest keeps 3 GiB.
        assert_eq!(next_target(8 * GIB, 8 * GIB, GIB, 6 * GIB, GIB, IDLE), Some(3 * GIB));
        assert_eq!(next_target(8 * GIB, 8 * GIB, GIB, 6 * GIB, GIB, IDLE - 1), None);
    }

    #[test]
    fn grows_straight_to_max_when_guest_runs_low() {
        assert_eq!(next_target(3 * GIB, 8 * GIB, GIB, 256 * MIB, GIB, 0), Some(8 * GIB));
        assert_eq!(next_target(8 * GIB, 8 * GIB, GIB, 0, GIB, 0), None, "already at max");
    }

    #[test]
    fn holds_steady_inside_the_band_and_respects_min() {
        assert_eq!(next_target(3 * GIB, 8 * GIB, GIB, GIB + 100 * MIB, GIB, IDLE), None);
        assert_eq!(next_target(2 * GIB, 8 * GIB, 2 * GIB, 4 * GIB, GIB, IDLE), None, "clamped at min");
    }

    #[test]
    fn parses_agent_reply() {
        assert_eq!(parse_meminfo("8124484 7765612 7776972 144844"), Some((7765612 * 1024, 144844 * 1024)));
        assert_eq!(parse_meminfo("error: x"), None);
    }
}

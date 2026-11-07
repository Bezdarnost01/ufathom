//! How much of a sample must agree with a candidate value.
//!
//! The thresholds are proportional, with an absolute floor. A real game has
//! 50k to 500k objects, and a few percent of them are freed, under
//! construction or damaged in ways no layout explains; a wrong offset, on the
//! other hand, agrees with a small share of the sample at best. So the bar is a
//! percentage well above any wrong offset (95% for names and structure, 90%
//! for property payloads, which have fewer instances and more variants), and
//! the floor lets a sample that is too small for a percentage to mean anything
//! keep a handful of outliers.

use crate::limits::{FAILURE_FLOOR_DIVISOR, TOLERATED_FAILURES};

/// Whether `matched` of `total` items agree at `percent`, or all but a few do.
///
/// The few are the larger of the share the percentage leaves and a floor of
/// [`TOLERATED_FAILURES`] items, capped at one in [`FAILURE_FLOOR_DIVISOR`] of
/// the sample so that a tiny sample cannot be waved through.
pub(crate) fn agrees(matched: usize, total: usize, percent: usize) -> bool {
    if matched > total {
        return false;
    }
    let failures = total - matched;
    let proportional = total * 100usize.saturating_sub(percent) / 100;
    let floor = (total / FAILURE_FLOOR_DIVISOR).min(TOLERATED_FAILURES);
    failures <= proportional.max(floor)
}

#[cfg(test)]
mod tests {
    use super::agrees;

    #[test]
    fn a_percentage_with_a_floor() {
        // 1000 items at 95%: 50 may fail.
        assert!(agrees(950, 1000, 95));
        assert!(!agrees(949, 1000, 95));
        // 100 items at 99%: the proportional share is 1, the floor 4.
        assert!(agrees(96, 100, 99));
        assert!(!agrees(95, 100, 99));
        // A tiny sample keeps no outliers.
        assert!(!agrees(0, 10, 99));
        assert!(agrees(10, 10, 99));
        assert!(!agrees(11, 10, 99));
    }
}

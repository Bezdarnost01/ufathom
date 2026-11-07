//! UTC time as text, without a date library.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds in a day.
const SECONDS_PER_DAY: u64 = 86_400;

/// A calendar date and time of day in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Civil {
    year: i64,
    month: u32,
    day: u32,
    hour: u64,
    minute: u64,
    second: u64,
    millisecond: u32,
}

/// Splits a time into calendar fields. A time before 1970 is shown as 1970.
fn civil(time: SystemTime) -> Civil {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since_epoch.as_secs();
    let days = i64::try_from(seconds / SECONDS_PER_DAY).unwrap_or(i64::MAX / 2);
    let of_day = seconds % SECONDS_PER_DAY;
    let (year, month, day) = civil_from_days(days);
    Civil {
        year,
        month,
        day,
        hour: of_day / 3600,
        minute: of_day % 3600 / 60,
        second: of_day % 60,
        millisecond: since_epoch.subsec_millis(),
    }
}

/// Days since 1970-01-01 to a proleptic Gregorian date.
///
/// This is Howard Hinnant's `civil_from_days`, valid for any day count.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days.saturating_add(719_468);
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * month_index + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    })
    .unwrap_or(1);
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `2026-11-07T10:15:30.123Z`, for files that describe a run.
#[must_use]
pub fn rfc3339(time: SystemTime) -> String {
    let c = civil(time);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.millisecond
    )
}

/// `20261107-101530`, for folder names that sort by time.
#[must_use]
pub fn compact(time: SystemTime) -> String {
    let c = civil(time);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(seconds: u64, millis: u32) -> SystemTime {
        UNIX_EPOCH + Duration::new(seconds, millis * 1_000_000)
    }

    #[test]
    fn the_epoch() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(compact(UNIX_EPOCH), "19700101-000000");
    }

    #[test]
    fn a_known_instant() {
        // 2026-11-07T10:15:30Z
        assert_eq!(rfc3339(at(1_794_046_530, 123)), "2026-11-07T10:15:30.123Z");
        assert_eq!(compact(at(1_794_046_530, 0)), "20261107-101530");
    }

    #[test]
    fn leap_days_and_year_ends() {
        // 2000-02-29T12:00:00Z and 2100-03-01T00:00:00Z (2100 is not a leap year).
        assert_eq!(rfc3339(at(951_825_600, 0)), "2000-02-29T12:00:00.000Z");
        assert_eq!(rfc3339(at(4_107_542_400, 0)), "2100-03-01T00:00:00.000Z");
        assert_eq!(rfc3339(at(1_767_225_599, 999)), "2025-12-31T23:59:59.999Z");
    }

    #[test]
    fn a_time_before_the_epoch_does_not_panic() {
        let before = UNIX_EPOCH - Duration::from_secs(10);
        assert_eq!(rfc3339(before), "1970-01-01T00:00:00.000Z");
    }
}

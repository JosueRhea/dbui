//! The current UTC time, written the way a timestamp column reads.
//!
//! This crate has no date library -- values arrive from the driver already
//! decoded, so nothing above the port has ever needed to *make* a timestamp.
//! The detail panel's calendar button does: offering "now" means producing a
//! literal the engine will accept, and `2026-09-14 18:04:22` is the one every
//! engine dbui speaks does.
//!
//! UTC, deliberately. The grid renders `timestamptz` in UTC too, so a value
//! typed by this button and a value read back from the server agree; taking
//! the machine's local zone would make them differ by the offset and look
//! like the server had rewritten it.

use std::time::{SystemTime, UNIX_EPOCH};

const SECONDS_PER_DAY: i64 = 86_400;

/// `YYYY-MM-DD HH:MM:SS`, or just `YYYY-MM-DD` when `date_only`.
pub(crate) fn now_utc(date_only: bool) -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        // A clock set before 1970 is not worth a failure path: the button
        // still has to produce something, and the epoch is a legal timestamp.
        .unwrap_or(0);
    format_utc(seconds, date_only)
}

/// Format a Unix timestamp in seconds as a UTC literal.
pub(crate) fn format_utc(seconds: i64, date_only: bool) -> String {
    // `div_euclid` rather than `/`: for a timestamp before the epoch, integer
    // division truncates towards zero and would put the day one too late.
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    if date_only {
        return format!("{year:04}-{month:02}-{day:02}");
    }
    let rest = seconds.rem_euclid(SECONDS_PER_DAY);
    let (hour, minute, second) = (rest / 3600, (rest % 3600) / 60, rest % 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// Days since 1970-01-01 to a civil `(year, month, day)`.
///
/// Howard Hinnant's `civil_from_days`, which is exact for the whole proleptic
/// Gregorian calendar and has no table of month lengths to get wrong. The
/// shifted era starts the year in March, which is what makes the leap day the
/// last day of the year instead of a special case in the middle of it.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Re-base onto 0000-03-01, the start of an era.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11], March = 0
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32; // [1, 31]
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32; // [1, 12]
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_the_first_of_january() {
        assert_eq!(format_utc(0, false), "1970-01-01 00:00:00");
        assert_eq!(format_utc(0, true), "1970-01-01");
    }

    #[test]
    fn seconds_become_a_wall_clock() {
        // 2023-11-14T22:13:20Z -- a round number with every field non-zero.
        assert_eq!(format_utc(1_700_000_000, false), "2023-11-14 22:13:20");
    }

    /// The reason the calendar is computed rather than tabulated.
    #[test]
    fn a_leap_day_is_the_day_it_says() {
        assert_eq!(format_utc(951_782_400, true), "2000-02-29");
        assert_eq!(format_utc(1_709_164_800, true), "2024-02-29");
        // 1900 was not a leap year; the day after 1900-02-28 is March.
        assert_eq!(format_utc(-2_203_891_200, true), "1900-03-01");
    }

    /// Truncating division would report the day *after* the right one for
    /// anything before 1970, and the hour as a negative number.
    #[test]
    fn a_timestamp_before_the_epoch_still_lands_on_a_real_date() {
        assert_eq!(format_utc(-1, false), "1969-12-31 23:59:59");
        assert_eq!(format_utc(-86_400, false), "1969-12-31 00:00:00");
    }

    #[test]
    fn now_is_shaped_like_a_timestamp() {
        let stamp = now_utc(false);
        assert_eq!(stamp.len(), 19, "{stamp}");
        assert_eq!(now_utc(true).len(), 10);
        // Anything this side of 2001 means the clock, not the formatter.
        assert!(stamp.starts_with("20"), "{stamp}");
    }
}
